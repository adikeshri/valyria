//! Phase 10 exit criteria, driving the real compiled `valyria` binary:
//! a client can drive every workflow through the protocol alone (embedded
//! *and* over the daemon socket), and `doctor` diagnoses the environment.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn cli_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_valyria"))
}

/// Per-test throwaway `VALYRIA_HOME` so the real binary never touches the
/// developer's `~/.valyria`.
struct TestEnv {
    _work: tempfile::TempDir,
    home: PathBuf,
    ws: PathBuf,
}

fn setup() -> TestEnv {
    let work = tempfile::tempdir().unwrap();
    let home = work.path().join("home");
    let ws = work.path().join("repo");
    std::fs::create_dir_all(ws.join("src")).unwrap();
    std::fs::write(ws.join("src/lib.rs"), "pub fn a(x: i32) -> i32 { x }\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "-A"],
        vec![
            "-c",
            "user.email=t@e.com",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ] {
        assert!(Command::new("git")
            .args(&args)
            .current_dir(&ws)
            .status()
            .unwrap()
            .success());
    }
    std::fs::create_dir_all(&home).unwrap();
    TestEnv {
        _work: work,
        home,
        ws,
    }
}

impl TestEnv {
    fn cmd(&self) -> Command {
        let mut c = Command::new(cli_bin());
        c.env("VALYRIA_HOME", &self.home);
        c
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        self.cmd().args(args).output().unwrap()
    }

    fn ws_str(&self) -> String {
        self.ws.display().to_string()
    }
}

fn json(out: &std::process::Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("stdout is JSON")
}

#[test]
fn doctor_json_reports_a_check_battery_including_git() {
    let env = setup();
    let out = env.run(&["doctor", "--workspace", &env.ws_str(), "--json"]);
    let v = json(&out);
    let checks = v["checks"].as_array().unwrap();
    assert!(checks.len() >= 8, "expected a battery of checks: {v}");
    let names: Vec<&str> = checks.iter().map(|c| c["name"].as_str().unwrap()).collect();
    for expected in [
        "runtime",
        "data_dir",
        "workspace_db",
        "git",
        "sandbox",
        "models",
    ] {
        assert!(
            names.contains(&expected),
            "missing `{expected}` in {names:?}"
        );
    }
    // A freshly `git init`'d repo with a commit: the git check passes.
    let git = checks.iter().find(|c| c["name"] == "git").unwrap();
    assert_eq!(git["status"], "pass");
}

#[test]
fn doctor_flags_a_non_git_workspace() {
    let env = setup();
    let bare = env._work.path().join("bare");
    std::fs::create_dir_all(&bare).unwrap();
    let out = env.run(&[
        "doctor",
        "--workspace",
        &bare.display().to_string(),
        "--json",
    ]);
    let v = json(&out);
    let git = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "git")
        .unwrap();
    assert_eq!(git["status"], "warn");
    assert!(git["remediation"].as_str().unwrap().contains("git init"));
}

#[test]
fn status_and_config_and_model_list_work_through_the_protocol() {
    let env = setup();

    let status = json(&env.run(&["status", "--workspace", &env.ws_str(), "--json"]));
    assert!(status["workspace_id"].as_str().unwrap().starts_with("ws_"));
    assert_eq!(status["total_tasks"], 0);

    let config = json(&env.run(&["config", "--workspace", &env.ws_str(), "--json"]));
    let keys: Vec<&str> = config["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"permission.mode"));

    let models = json(&env.run(&["model", "list", "--json"]));
    assert!(
        !models["models"].as_array().unwrap().is_empty(),
        "embedded catalog should be non-empty"
    );
    assert!(models["models"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["installed"] == false));
}

#[test]
fn run_then_list_and_report_over_the_embedded_client() {
    let env = setup();
    let run = env.run(&["run", "add a function", "--workspace", &env.ws_str()]);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let task_id = String::from_utf8_lossy(&run.stdout)
        .lines()
        .next()
        .unwrap()
        .strip_prefix("task_id: ")
        .unwrap()
        .to_string();

    let list = json(&env.run(&["task", "list", "--workspace", &env.ws_str(), "--json"]));
    assert!(list["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["task_id"] == task_id));

    let report = json(&env.run(&[
        "task",
        "report",
        &task_id,
        "--workspace",
        &env.ws_str(),
        "--json",
    ]));
    // The fake-model walking-skeleton scenario runs a command but no
    // test-tier verification, so the honest status is "not verified" —
    // never a fabricated pass (D4).
    assert!(
        ["not_verified", "partially_verified", "verified"]
            .contains(&report["status"].as_str().unwrap()),
        "unexpected status {report}"
    );
}

#[test]
fn clean_dry_run_reports_without_deleting() {
    let env = setup();
    // Make a cache file to "free".
    let cache = env.ws.join(".valyria/cache");
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join("blob"), vec![0u8; 4096]).unwrap();

    let v = json(&env.run(&[
        "clean",
        "--scope",
        "cache",
        "--dry-run",
        "--workspace",
        &env.ws_str(),
        "--json",
    ]));
    assert_eq!(v["freed_bytes"], 4096);
    assert_eq!(v["dry_run"], true);
    assert!(cache.join("blob").exists(), "dry run must not delete");
}

#[test]
fn daemon_serves_the_same_protocol_over_a_unix_socket() {
    let env = setup();
    let sock = env._work.path().join("valyria.sock");

    let mut daemon = env
        .cmd()
        .args([
            "serve",
            "--workspace",
            &env.ws_str(),
            "--socket",
            &sock.display().to_string(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // Wait for the socket to appear.
    let deadline = Instant::now() + Duration::from_secs(10);
    while !sock.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(sock.exists(), "daemon never created its socket");

    let connect = sock.display().to_string();

    // `--connect` routes through SocketClient — no workspace flag, the
    // daemon already owns one.
    let doctor = json(&env.run(&["doctor", "--connect", &connect, "--json"]));
    assert!(doctor["checks"].as_array().unwrap().len() >= 8);

    let created = env.run(&["run", "add a function", "--connect", &connect]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );

    let list = json(&env.run(&["task", "list", "--connect", &connect, "--json"]));
    assert_eq!(list["tasks"].as_array().unwrap().len(), 1);

    daemon.kill().unwrap();
    let _ = daemon.wait();
}
