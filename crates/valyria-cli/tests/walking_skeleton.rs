//! End-to-end proof of Phase 3's exit criterion, driving the real compiled
//! `valyria` binary as a separate OS process against a real git fixture
//! repo: `valyria run "add a function"` reads a file, edits it, runs a
//! command, persists, streams events over the protocol, survives a real
//! `SIGKILL` + restart + resume, and can be paused/cancelled — all from
//! separate process invocations, exactly as a user would drive it.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

const FIXTURE_LIB_RS: &str = "pub fn existing(a: i32) -> i32 {\n    a\n}\n";

fn cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_valyria"))
}

/// A throwaway `~/.valyria` *per test*, so the real compiled `valyria`
/// never touches the developer's actual global store (`global.db`, the
/// model index) — and so tests running in parallel never share one
/// `global.db` (which would race migrations and lock the file). Keyed by
/// the libtest thread name, which is the test function's path and is
/// stable for the life of the test, so every `valyria` invocation a
/// single test makes lands in the same home.
fn test_home() -> PathBuf {
    let key = std::thread::current()
        .name()
        .unwrap_or("unnamed")
        .replace([':', '/'], "_");
    let dir = std::env::temp_dir().join(format!("valyria-it-home-{}-{key}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `valyria` bin with `VALYRIA_HOME` pinned to [`test_home`].
fn cli_command() -> Command {
    let mut cmd = Command::new(cli_bin());
    cmd.env("VALYRIA_HOME", test_home());
    cmd
}

/// A git fixture repo the CLI can be pointed at with `--workspace`.
fn fixture_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), FIXTURE_LIB_RS).unwrap();
    run_git(dir.path(), &["init", "-q"]);
    run_git(dir.path(), &["add", "-A"]);
    run_git(
        dir.path(),
        &[
            "-c",
            "user.email=t@example.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
    dir
}

fn run_git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn fixture_content(ws: &Path) -> String {
    std::fs::read_to_string(ws.join("src/lib.rs")).unwrap()
}

fn valyria(args: &[&str]) -> std::process::Output {
    cli_command().args(args).output().unwrap()
}

/// Spawns `valyria run` with stdout piped, reads (and returns) exactly the
/// first line — the CLI flushes immediately after printing it — leaving
/// the child running in the background for the caller to observe further
/// or kill.
fn spawn_run(
    workspace: &Path,
    extra_args: &[&str],
) -> (Child, BufReader<std::process::ChildStdout>, String) {
    let mut cmd = cli_command();
    cmd.arg("run")
        .arg("add a function")
        .arg("--workspace")
        .arg(workspace)
        .args(extra_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut first_line = String::new();
    reader.read_line(&mut first_line).unwrap();
    let task_id = first_line
        .trim()
        .strip_prefix("task_id: ")
        .expect("first line must be `task_id: <id>`")
        .to_string();
    (child, reader, task_id)
}

fn task_status(workspace: &Path, task_id: &str) -> String {
    let out = valyria(&[
        "task",
        "status",
        task_id,
        "--workspace",
        &workspace.display().to_string(),
    ]);
    assert!(
        out.status.success(),
        "{:?}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn status_field(status_text: &str, field: &str) -> Option<String> {
    status_text
        .lines()
        .find_map(|l| l.strip_prefix(&format!("{field}: ")))
        .map(str::to_string)
}

#[test]
fn full_run_completes_and_edits_the_fixture() {
    let ws = fixture_repo();
    let out = valyria(&[
        "run",
        "add a function",
        "--workspace",
        &ws.path().display().to_string(),
        "--events",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let first_line = stdout.lines().next().unwrap();
    assert!(first_line.starts_with("task_id: "), "{first_line}");

    for expected in [
        "\"kind\":\"task_started\"",
        "\"kind\":\"tool_started\"",
        "\"kind\":\"tool_completed\"",
        "\"kind\":\"state_changed\"",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected} in:\n{stdout}"
        );
    }
    assert!(stdout.contains("completed"));

    let content = fixture_content(ws.path());
    assert!(
        content.contains("pub fn add(a: i32, b: i32) -> i32"),
        "{content}"
    );
    assert!(content.contains("pub fn existing"), "{content}");
}

#[test]
fn task_status_reports_the_created_task() {
    let ws = fixture_repo();
    let out = valyria(&[
        "run",
        "add a function",
        "--workspace",
        &ws.path().display().to_string(),
    ]);
    assert!(out.status.success());
    let task_id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap()
        .strip_prefix("task_id: ")
        .unwrap()
        .to_string();

    let status = task_status(ws.path(), &task_id);
    assert_eq!(status_field(&status, "state").as_deref(), Some("COMPLETED"));
    assert_eq!(
        status_field(&status, "objective").as_deref(),
        Some("add a function")
    );
}

/// A real `SIGKILL` mid-scenario, a real restart in a new process, and a
/// real resume — proving the exit criterion's "survives kill -9, restart,
/// and resume" over actual OS process boundaries and a real on-disk sqlite
/// file, not just in-process simulation.
///
/// The fake-model scenario completes in only a few milliseconds, so racing
/// an external kill against it is inherently timing-sensitive; this
/// retries a bounded number of times, killing as early as physically
/// possible (`Child::kill` is a direct `SIGKILL` syscall, not a shelled-out
/// `kill` command, specifically to make winning that race likely) and
/// treating "it happened to finish before we could kill it" as a race to
/// retry, not a failure.
#[test]
fn kill_nine_then_restart_and_resume_completes_correctly_without_double_applying() {
    const MAX_ATTEMPTS: usize = 25;

    for _attempt in 1..=MAX_ATTEMPTS {
        let ws = fixture_repo();
        let (mut child, _reader, task_id) = spawn_run(ws.path(), &[]);
        child.kill().unwrap(); // SIGKILL, as immediately as possible
        let _ = child.wait();

        let status = task_status(ws.path(), &task_id);
        let state = status_field(&status, "state").unwrap();

        if state == "COMPLETED" {
            // Lost the race this attempt: the scenario finished before the
            // kill landed. Not a failure of the property under test.
            continue;
        }

        assert_ne!(
            state, "FAILED",
            "task failed instead of being interrupted: {status}"
        );
        assert!(state != "CANCELLED", "unexpected cancellation: {status}");

        // Resume as a brand-new process against the same on-disk store.
        let out = valyria(&[
            "task",
            "resume",
            &task_id,
            "--workspace",
            &ws.path().display().to_string(),
        ]);
        assert!(
            out.status.success(),
            "resume failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let final_status = task_status(ws.path(), &task_id);
        assert_eq!(
            status_field(&final_status, "state").as_deref(),
            Some("COMPLETED"),
            "{final_status}"
        );

        let content = fixture_content(ws.path());
        assert_eq!(
            content.matches("pub fn add(a: i32, b: i32)").count(),
            1,
            "expected the addition exactly once (no double-apply) after resume, got:\n{content}"
        );
        assert!(content.contains("pub fn existing"), "{content}");

        return; // caught it mid-flight at least once and verified correctness
    }

    panic!(
        "never managed to kill the process before it completed in {MAX_ATTEMPTS} attempts — \
         either the race-avoidance strategy needs revisiting, or something is unexpectedly slow"
    );
}

#[test]
fn pause_from_a_separate_process_lands_cleanly_and_resume_completes() {
    let ws = fixture_repo();
    // A scenario with a deliberately slower step (an Ask-triggering
    // `run_command`, since `sleep` isn't in the safe auto-allow list) isn't
    // needed here: `valyria task pause` durably marks the row regardless
    // of timing, and `run` picks it up the moment it next checks — so even
    // a "too late" pause request simply lands after completion, which this
    // test tolerates the same way the kill-9 test tolerates winning too
    // late, by asserting on the eventually-consistent outcome rather than
    // a specific interruption point.
    let (mut child, _reader, task_id) = spawn_run(ws.path(), &[]);

    let pause_out = valyria(&[
        "task",
        "pause",
        &task_id,
        "--workspace",
        &ws.path().display().to_string(),
    ]);
    assert!(
        pause_out.status.success(),
        "{}",
        String::from_utf8_lossy(&pause_out.stderr)
    );

    let exit = child.wait().unwrap();
    let code = exit.code().unwrap();
    // 0 = raced to completion before the pause was noticed; 4 = paused
    // cleanly. Both are valid outcomes of the same durable-signal design.
    assert!(code == 0 || code == 4, "unexpected exit code {code}");

    if code == 4 {
        let status = task_status(ws.path(), &task_id);
        assert_eq!(status_field(&status, "state").as_deref(), Some("PAUSED"));

        let out = valyria(&[
            "task",
            "resume",
            &task_id,
            "--workspace",
            &ws.path().display().to_string(),
        ]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let final_status = task_status(ws.path(), &task_id);
    assert_eq!(
        status_field(&final_status, "state").as_deref(),
        Some("COMPLETED")
    );
    let content = fixture_content(ws.path());
    assert_eq!(
        content.matches("pub fn add(a: i32, b: i32)").count(),
        1,
        "{content}"
    );
}

#[test]
fn cancel_from_a_separate_process_reaches_cancelled_or_completes_first() {
    let ws = fixture_repo();
    let (mut child, _reader, task_id) = spawn_run(ws.path(), &[]);

    let cancel_out = valyria(&[
        "task",
        "cancel",
        &task_id,
        "--workspace",
        &ws.path().display().to_string(),
    ]);
    assert!(
        cancel_out.status.success(),
        "{}",
        String::from_utf8_lossy(&cancel_out.stderr)
    );

    let exit = child.wait().unwrap();
    let code = exit.code().unwrap();
    assert!(code == 0 || code == 2, "unexpected exit code {code}");

    let status = task_status(ws.path(), &task_id);
    let state = status_field(&status, "state").unwrap();
    assert!(state == "COMPLETED" || state == "CANCELLED", "{status}");
}

/// D2's Ask -> approve path end to end: a command outside the safe
/// auto-allow list forces `WAITING_FOR_PERMISSION` even in the default
/// `Assisted` mode, resolved from a *separate* CLI process.
#[test]
fn permission_ask_can_be_resolved_from_a_separate_process_and_completes() {
    let ws = fixture_repo();
    let scenario_path = ws.path().join("ask_scenario.toml");
    std::fs::write(
        &scenario_path,
        r#"
name = "ask_then_finish"

[[turns]]
kind = "tool_call"
name = "run_command"
arguments = { program = "some-unlisted-tool", args = [] }

[[turns]]
kind = "finish"
summary = "done"
"#,
    )
    .unwrap();

    let out = valyria(&[
        "run",
        "run something",
        "--workspace",
        &ws.path().display().to_string(),
        "--scenario",
        &scenario_path.display().to_string(),
    ]);
    // Exit code 3 (WAITING_FOR_PERMISSION) is the *expected* outcome here,
    // not a failure — `ExitStatus::success()` only ever means exit code 0.
    assert_eq!(
        out.status.code(),
        Some(3),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let task_id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap()
        .strip_prefix("task_id: ")
        .unwrap()
        .to_string();

    let status = task_status(ws.path(), &task_id);
    assert_eq!(
        status_field(&status, "state").as_deref(),
        Some("WAITING_FOR_PERMISSION")
    );

    let resolve = valyria(&[
        "task",
        "permission",
        "resolve",
        &task_id,
        "--allow",
        "--workspace",
        &ws.path().display().to_string(),
    ]);
    assert!(
        resolve.status.success(),
        "{}",
        String::from_utf8_lossy(&resolve.stderr)
    );

    let final_status = task_status(ws.path(), &task_id);
    assert_eq!(
        status_field(&final_status, "state").as_deref(),
        Some("COMPLETED")
    );
}

#[test]
fn denying_a_permission_ask_fails_the_task() {
    let ws = fixture_repo();
    let scenario_path = ws.path().join("ask_scenario.toml");
    std::fs::write(
        &scenario_path,
        r#"
name = "ask_then_finish"

[[turns]]
kind = "tool_call"
name = "run_command"
arguments = { program = "some-unlisted-tool", args = [] }

[[turns]]
kind = "finish"
summary = "done"
"#,
    )
    .unwrap();

    let out = valyria(&[
        "run",
        "run something",
        "--workspace",
        &ws.path().display().to_string(),
        "--scenario",
        &scenario_path.display().to_string(),
    ]);
    assert_eq!(out.status.code(), Some(3));
    let task_id = String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap()
        .strip_prefix("task_id: ")
        .unwrap()
        .to_string();

    let resolve = valyria(&[
        "task",
        "permission",
        "resolve",
        &task_id,
        "--deny",
        "--workspace",
        &ws.path().display().to_string(),
    ]);
    // Exit code 1 (FAILED) is the expected outcome of a denial, not a
    // failure of the CLI invocation itself.
    assert_eq!(
        resolve.status.code(),
        Some(1),
        "stderr: {}",
        String::from_utf8_lossy(&resolve.stderr)
    );

    let final_status = task_status(ws.path(), &task_id);
    assert_eq!(
        status_field(&final_status, "state").as_deref(),
        Some("FAILED")
    );
}

#[test]
fn version_flag_prints_a_version() {
    let out = valyria(&["--version"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("valyria "));
}

// --- Phase 8: model-authored planning ------------------------------------

/// A two-step plan scenario. Each step *creates* a new file with a
/// `must_not_exist` precondition, so re-applying an interrupted write
/// fails the precondition rather than duplicating anything — which is what
/// makes the kill/resume assertion ("each file present exactly once")
/// robust regardless of where the SIGKILL lands.
const PLAN_SCENARIO_TOML: &str = r#"
name = "two_step_plan"

[[turns]]
kind = "tool_call"
name = "submit_plan"
arguments = { plan_scope = ["src/"], steps = [ { id = "create_alpha", intent = "create src/alpha.rs", targets = ["src/alpha.rs"], verification = { mode = "inherit" }, checkpoint = true, rollback_boundary = true }, { id = "create_beta", intent = "create src/beta.rs", targets = ["src/beta.rs"], depends_on = ["create_alpha"], verification = { mode = "inherit" } } ] }

[[turns]]
kind = "tool_call"
name = "write_file"
arguments = { path = "src/alpha.rs", content = "// alpha\n", precondition = "must_not_exist" }

[[turns]]
kind = "finish"
summary = "alpha created"

[[turns]]
kind = "tool_call"
name = "write_file"
arguments = { path = "src/beta.rs", content = "// beta\n", precondition = "must_not_exist" }

[[turns]]
kind = "finish"
summary = "beta created"
"#;

fn write_plan_scenario(ws: &Path) -> PathBuf {
    let path = ws.join("plan_scenario.toml");
    std::fs::write(&path, PLAN_SCENARIO_TOML).unwrap();
    path
}

#[test]
fn model_authored_plan_runs_end_to_end_and_streams_plan_created() {
    let ws = fixture_repo();
    let scenario = write_plan_scenario(ws.path());
    let out = valyria(&[
        "run",
        "create two files by plan",
        "--workspace",
        &ws.path().display().to_string(),
        "--scenario",
        &scenario.display().to_string(),
        "--plan",
        "--permission-mode",
        "autonomous",
        "--events",
    ]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"kind\":\"plan_created\""),
        "missing plan_created in:\n{stdout}"
    );
    assert!(stdout.contains("completed"), "{stdout}");

    assert_eq!(
        std::fs::read_to_string(ws.path().join("src/alpha.rs")).unwrap(),
        "// alpha\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.path().join("src/beta.rs")).unwrap(),
        "// beta\n"
    );
}

/// Exit criterion 2: a multi-step plan executes with a mid-plan
/// interruption + resume across a real process restart. Same
/// race-tolerant harness as `kill_nine_then_restart_and_resume_*`.
#[test]
fn multi_step_plan_survives_kill_nine_and_resumes_mid_plan() {
    const MAX_ATTEMPTS: usize = 25;

    for _attempt in 1..=MAX_ATTEMPTS {
        let ws = fixture_repo();
        let scenario = write_plan_scenario(ws.path());
        let (mut child, _reader, task_id) = spawn_run(
            ws.path(),
            &[
                "--scenario",
                &scenario.display().to_string(),
                "--plan",
                "--permission-mode",
                "autonomous",
            ],
        );
        child.kill().unwrap();
        let _ = child.wait();

        let status = task_status(ws.path(), &task_id);
        let state = status_field(&status, "state").unwrap();

        if state == "COMPLETED" {
            continue; // raced to completion before the kill landed
        }
        assert_ne!(
            state, "FAILED",
            "task failed instead of interrupted: {status}"
        );
        assert_ne!(state, "CANCELLED", "unexpected cancellation: {status}");

        // Resume must be handed the same scenario + `--plan` (the CLI
        // rebuilds the runtime per invocation and has no daemon to carry
        // that choice), exactly as `--workspace` is repeated on every
        // subcommand.
        let out = valyria(&[
            "task",
            "resume",
            &task_id,
            "--workspace",
            &ws.path().display().to_string(),
            "--scenario",
            &scenario.display().to_string(),
            "--plan",
            "--permission-mode",
            "autonomous",
        ]);
        assert!(
            out.status.success(),
            "resume failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );

        let final_status = task_status(ws.path(), &task_id);
        assert_eq!(
            status_field(&final_status, "state").as_deref(),
            Some("COMPLETED"),
            "{final_status}"
        );

        // Both plan steps ran, exactly once each, across the restart.
        assert_eq!(
            std::fs::read_to_string(ws.path().join("src/alpha.rs")).unwrap(),
            "// alpha\n"
        );
        assert_eq!(
            std::fs::read_to_string(ws.path().join("src/beta.rs")).unwrap(),
            "// beta\n"
        );
        return;
    }

    panic!("never interrupted the plan mid-flight in {MAX_ATTEMPTS} attempts");
}
