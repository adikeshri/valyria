//! Integration coverage for the repository read surface added in protocol
//! 1.2.0 (CORE-INTERFACE gap G3): `git_status`, `git_diff`, `git_log`,
//! `git_branches`, `search_query`, `index_status` over `EmbeddedClient`.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use valyria_app::{EmbeddedClient, Runtime, RuntimeConfig};
use valyria_protocol::{
    Client as _, Empty, GitDiffRequest, GitLogRequest, Request, Response, SearchQueryRequest,
};

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git must be installed for these tests");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A git workspace: one commit adding `src/lib.rs`, then an uncommitted
/// edit plus a new untracked file. Returns `(Runtime, workspace dir,
/// data dir)` — the tempdirs are leaked to keep them alive for the test.
async fn workspace() -> (Arc<Runtime>, tempfile::TempDir) {
    let ws = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let root = ws.path();

    git(root, &["init", "-q", "-b", "main"]);
    git(root, &["config", "user.email", "t@example.com"]);
    git(root, &["config", "user.name", "T"]);
    git(root, &["config", "commit.gpgsign", "false"]);
    std::fs::create_dir(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn tokenize(s: &str) -> usize {\n    s.split_whitespace().count()\n}\n",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-q", "-m", "initial commit"]);

    // An unstaged edit (length-changing so gix cannot treat it as racy)
    // and a brand-new untracked file.
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn tokenize(input: &str) -> usize {\n    input.split_whitespace().count() + 1\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("NOTES.md"), "scratch\n").unwrap();

    let config = RuntimeConfig::new(root).with_data_dir(data.path().join("d"));
    std::mem::forget(data);
    (Arc::new(Runtime::open(config).await.unwrap()), ws)
}

#[tokio::test]
async fn git_status_reports_the_branch_and_the_working_changes() {
    let (rt, _ws) = workspace().await;
    let client = EmbeddedClient::new(rt);

    let Response::GitStatus(s) = client.call(Request::GitStatus(Empty {})).await else {
        panic!("expected GitStatus");
    };
    assert_eq!(s.branch.as_deref(), Some("main"));
    assert!(!s.detached);
    assert!(s.head_commit.is_some());

    let modified = s
        .files
        .iter()
        .find(|f| f.path == "src/lib.rs")
        .expect("src/lib.rs should be listed");
    assert_eq!(modified.kind, "modified");
    assert!(!modified.staged);

    let untracked = s
        .files
        .iter()
        .find(|f| f.path == "NOTES.md")
        .expect("NOTES.md should be listed");
    assert_eq!(untracked.kind, "untracked");
}

#[tokio::test]
async fn git_diff_returns_unified_text_for_the_worktree() {
    let (rt, _ws) = workspace().await;
    let client = EmbeddedClient::new(rt);

    let Response::GitDiff(d) = client
        .call(Request::GitDiff(GitDiffRequest {
            path: None,
            staged: false,
        }))
        .await
    else {
        panic!("expected GitDiff");
    };
    assert!(d.unified.contains("diff --git a/src/lib.rs b/src/lib.rs"));
    assert!(d.unified.contains("-pub fn tokenize(s: &str)"));
    assert!(d.unified.contains("+pub fn tokenize(input: &str)"));
    assert!(!d.truncated);

    // A path filter restricts the output.
    let Response::GitDiff(only) = client
        .call(Request::GitDiff(GitDiffRequest {
            path: Some("NOTES.md".into()),
            staged: false,
        }))
        .await
    else {
        panic!("expected GitDiff");
    };
    assert!(only.unified.contains("NOTES.md"));
    assert!(!only.unified.contains("src/lib.rs"));
}

#[tokio::test]
async fn git_log_and_branches_read_history() {
    let (rt, _ws) = workspace().await;
    let client = EmbeddedClient::new(rt);

    let Response::GitLog(l) = client
        .call(Request::GitLog(GitLogRequest { limit: Some(10) }))
        .await
    else {
        panic!("expected GitLog");
    };
    assert_eq!(l.commits.len(), 1);
    assert_eq!(l.commits[0].message, "initial commit");
    assert_eq!(l.commits[0].author_email, "t@example.com");
    assert!(l.commits[0].parents.is_empty());

    let Response::GitBranches(b) = client.call(Request::GitBranches(Empty {})).await else {
        panic!("expected GitBranches");
    };
    assert_eq!(b.branches.len(), 1);
    assert_eq!(b.branches[0].name, "main");
    assert!(b.branches[0].is_head);
}

#[tokio::test]
async fn index_status_is_none_until_reindex_then_search_returns_explained_hits() {
    let (rt, _ws) = workspace().await;
    let client = EmbeddedClient::new(rt.clone());

    // Nothing indexed yet.
    let Response::IndexStatus(before) = client.call(Request::IndexStatus(Empty {})).await else {
        panic!("expected IndexStatus");
    };
    assert_eq!(before.generation, None);

    // And search says so, with a real code, not a transport failure.
    let Response::Error(e) = client
        .call(Request::SearchQuery(SearchQueryRequest {
            query: "tokenize".into(),
            ..Default::default()
        }))
        .await
    else {
        panic!("expected an error before indexing");
    };
    assert_eq!(e.code, "search.not_indexed");

    // Build the index.
    rt.reindex().await.unwrap();

    let Response::IndexStatus(after) = client.call(Request::IndexStatus(Empty {})).await else {
        panic!("expected IndexStatus");
    };
    assert!(after.generation.is_some());
    assert!(after.file_count >= 1, "expected at least one indexed file");

    // Now search finds the symbol, and every hit's features sum to its score.
    let Response::SearchQuery(res) = client
        .call(Request::SearchQuery(SearchQueryRequest {
            query: "tokenize".into(),
            modes: vec!["lexical".into(), "symbol".into()],
            limit: Some(10),
            ..Default::default()
        }))
        .await
    else {
        panic!("expected SearchQuery");
    };
    assert!(!res.hits.is_empty(), "expected a hit for `tokenize`");
    let hit = res
        .hits
        .iter()
        .find(|h| h.path == "src/lib.rs")
        .expect("src/lib.rs should be a hit");
    assert!(!hit.explanation.features.is_empty());
    let summed: f64 = hit
        .explanation
        .features
        .iter()
        .map(|f| f.contribution)
        .sum();
    assert!(
        (summed - hit.score).abs() < 1e-9,
        "features {summed} must sum to score {}",
        hit.score
    );
    assert!(!res.modes_run.is_empty());
}

#[tokio::test]
async fn search_rejects_an_unknown_mode() {
    let (rt, _ws) = workspace().await;
    let client = EmbeddedClient::new(rt);

    let Response::Error(e) = client
        .call(Request::SearchQuery(SearchQueryRequest {
            query: "x".into(),
            modes: vec!["telepathy".into()],
            ..Default::default()
        }))
        .await
    else {
        panic!("expected an error");
    };
    assert_eq!(e.code, "search.unknown_mode");
}
