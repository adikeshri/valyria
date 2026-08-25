//! Integration tests against real spawned processes. Unix-only (`/bin/sh`),
//! matching the tiered platform support in the build plan — Windows
//! coverage for this crate lands with the sandbox/Windows tier work.

#![cfg(unix)]

use std::time::Duration;

use valyria_process::{run, CommandSpec, EndReason, EnvPolicy};
use valyria_util::CancellationToken;

fn sh(cwd: &std::path::Path, script: &str) -> CommandSpec {
    CommandSpec::new("/bin/sh", cwd).arg("-c").arg(script)
}

#[tokio::test]
async fn captures_stdout_and_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let spec = sh(dir.path(), "echo hello world");
    let result = run(&spec, CancellationToken::new()).await.unwrap();

    assert!(result.success());
    assert_eq!(result.exit_code, Some(0));
    assert_eq!(result.stdout.text.trim(), "hello world");
}

#[tokio::test]
async fn captures_nonzero_exit_code() {
    let dir = tempfile::tempdir().unwrap();
    let spec = sh(dir.path(), "exit 3");
    let result = run(&spec, CancellationToken::new()).await.unwrap();

    assert!(!result.success());
    assert_eq!(result.exit_code, Some(3));
    assert_eq!(result.end_reason, EndReason::Exited);
}

#[tokio::test]
async fn captures_stderr_separately_from_stdout() {
    let dir = tempfile::tempdir().unwrap();
    let spec = sh(dir.path(), "echo to-out; echo to-err 1>&2");
    let result = run(&spec, CancellationToken::new()).await.unwrap();

    assert_eq!(result.stdout.text.trim(), "to-out");
    assert_eq!(result.stderr.text.trim(), "to-err");
}

#[tokio::test]
async fn respects_working_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("marker.txt"), "present").unwrap();
    let spec = sh(dir.path(), "cat marker.txt");
    let result = run(&spec, CancellationToken::new()).await.unwrap();

    assert_eq!(result.stdout.text.trim(), "present");
}

#[tokio::test]
async fn wall_clock_timeout_kills_a_hanging_process() {
    let dir = tempfile::tempdir().unwrap();
    let spec = sh(dir.path(), "sleep 100").timeout(Duration::from_millis(300));

    let start = std::time::Instant::now();
    let result = run(&spec, CancellationToken::new()).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(result.end_reason, EndReason::TimedOut);
    assert!(
        elapsed < Duration::from_secs(5),
        "expected the process to be killed promptly, took {elapsed:?}"
    );
}

#[tokio::test]
async fn cancellation_kills_a_hanging_process_promptly() {
    let dir = tempfile::tempdir().unwrap();
    let spec = sh(dir.path(), "sleep 100").no_timeout();
    let cancel = CancellationToken::new();

    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel_clone.cancel();
    });

    let start = std::time::Instant::now();
    let result = run(&spec, cancel).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(result.end_reason, EndReason::Cancelled);
    assert!(
        elapsed < Duration::from_secs(5),
        "expected cancellation to kill the process promptly, took {elapsed:?}"
    );
}

#[tokio::test]
async fn idle_timeout_fires_when_no_new_output_arrives() {
    let dir = tempfile::tempdir().unwrap();
    // Prints once immediately, then goes quiet for far longer than the
    // idle timeout, while never hitting a wall-clock timeout (none set).
    let spec = sh(dir.path(), "echo start; sleep 100")
        .no_timeout()
        .idle_timeout(Duration::from_millis(300));

    let start = std::time::Instant::now();
    let result = run(&spec, CancellationToken::new()).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(result.end_reason, EndReason::IdleTimedOut);
    assert_eq!(result.stdout.text.trim(), "start");
    assert!(
        elapsed < Duration::from_secs(5),
        "expected idle timeout to fire well before the 100s sleep, took {elapsed:?}"
    );
}

#[tokio::test]
async fn kills_the_whole_process_group_not_just_the_shell() {
    // The shell backgrounds a grandchild sleep and waits on it; if only
    // the shell (the direct child) were killed, the grandchild would
    // become an orphan and keep running to completion. Process-group kill
    // must take it down too, so the overall run still finishes promptly.
    let dir = tempfile::tempdir().unwrap();
    let spec = sh(dir.path(), "sleep 100 & wait").timeout(Duration::from_millis(300));

    let start = std::time::Instant::now();
    let result = run(&spec, CancellationToken::new()).await.unwrap();
    let elapsed = start.elapsed();

    assert_eq!(result.end_reason, EndReason::TimedOut);
    assert!(
        elapsed < Duration::from_secs(5),
        "expected the grandchild to die with the group, took {elapsed:?}"
    );
}

#[tokio::test]
async fn output_beyond_the_cap_is_truncated_but_run_still_completes() {
    let dir = tempfile::tempdir().unwrap();
    let spec = sh(dir.path(), "head -c 200000 /dev/zero").max_output_bytes(1000);
    let result = run(&spec, CancellationToken::new()).await.unwrap();

    assert!(result.success());
    assert!(result.stdout.truncated);
    assert_eq!(result.stdout.total_bytes, 200_000);
    assert!(result.stdout.text.len() < 2000);
}

#[tokio::test]
async fn env_policy_wiring_env_clear_and_explicit_vars_only() {
    let dir = tempfile::tempdir().unwrap();
    let env = EnvPolicy::strict()
        .with_var("FOO", "bar")
        .build(&Default::default());
    let spec = sh(dir.path(), "echo [$FOO][$UNSET_VAR]").env(env);

    let result = run(&spec, CancellationToken::new()).await.unwrap();
    assert_eq!(result.stdout.text.trim(), "[bar][]");
}

#[tokio::test]
async fn nonexistent_program_is_a_typed_spawn_error() {
    let dir = tempfile::tempdir().unwrap();
    let spec = CommandSpec::new("/definitely/not/a/real/binary", dir.path());
    let err = run(&spec, CancellationToken::new()).await.unwrap_err();
    assert!(matches!(err, valyria_process::ProcessError::Spawn { .. }));
}
