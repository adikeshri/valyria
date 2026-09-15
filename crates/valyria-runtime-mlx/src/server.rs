//! `MlxServer`: spawns and supervises one `python -m mlx_lm.server` child
//! process. Structurally identical to `valyria-runtime-llamacpp::
//! LlamaServer` — same readiness-poll contract, same graceful-then-hard
//! shutdown, same log-tail-on-failure — deliberately, since both crates'
//! callers (`valyria-app`) treat every `LocalModelServer` the same way
//! regardless of which engine is actually running. Not
//! `#![forbid(unsafe_code)]` for the same reason as `LlamaServer`: a
//! graceful stop needs a raw `SIGTERM` before tokio's `kill_on_drop`
//! hard-kill.

use std::collections::HashMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use valyria_process::EnvPolicy;
use valyria_util::Backoff;

use crate::error::{MlxError, Result};

/// How long a `/health` readiness poll waits before giving up. Generous —
/// MLX lazily imports a fair amount of Python (transformers, huggingface_hub)
/// before it can even start loading weights, on top of the weights
/// themselves mapping in.
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(120);
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_TAIL_LINES: usize = 20;

pub struct MlxServerConfig {
    /// The provisioned venv's own `python` executable — invoked by
    /// absolute path rather than relying on `PATH`/`VIRTUAL_ENV`
    /// activation, so this never accidentally picks up a different
    /// Python (system, pyenv, …) than the one `mlx-lm` was installed
    /// into.
    pub python: PathBuf,
    /// A local directory containing the MLX model's `config.json`,
    /// tokenizer files, and `.safetensors` weights (Hugging Face layout —
    /// MLX has no single-file format the way GGUF is one) — resolved and
    /// downloaded ahead of time by the caller, exactly as `weights` is
    /// for `LlamaServer`.
    pub model_dir: PathBuf,
    /// Extra flags appended verbatim after the standard set.
    pub extra_args: Vec<String>,
    /// Where to append the child's interleaved stdout/stderr.
    pub log_path: PathBuf,
}

/// A running (or exited) `python -m mlx_lm.server` child, its assigned
/// loopback port, and a tail buffer of its own log for error messages.
/// `shutdown` is idempotent; if it's never called, `kill_on_drop` on the
/// underlying `tokio::process::Command` still reaps the child when this
/// value drops.
pub struct MlxServer {
    child: Mutex<Option<Child>>,
    pid: Option<u32>,
    port: u16,
    log_tail: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl MlxServer {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub async fn spawn(config: MlxServerConfig) -> Result<Self> {
        let port = free_port().map_err(MlxError::Spawn)?;
        if let Some(parent) = config.log_path.parent() {
            std::fs::create_dir_all(parent).map_err(MlxError::Spawn)?;
        }

        let mut cmd = Command::new(&config.python);
        // `python -m mlx_lm.server` (the dotted form) is deprecated by
        // mlx-lm itself as of 0.31.x in favor of the `mlx_lm server`
        // subcommand — confirmed live against a real installed 0.31.3 on
        // this machine (`--help` on the dotted form prints a deprecation
        // notice to stderr that would otherwise pollute the log tail on
        // every single boot; the subcommand form is silent).
        cmd.arg("-m")
            .arg("mlx_lm")
            .arg("server")
            .arg("--model")
            .arg(&config.model_dir)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            // The model's own tokenizer_config.json chat template,
            // matching `LlamaServer`'s `--jinja` (the GGUF's own
            // template) — never `valyria`'s own prompt formatting.
            .arg("--use-default-chat-template")
            .args(&config.extra_args);

        // Filtered-inherited env: `mlx_lm.server` needs a normal-looking
        // environment (HOME, for its huggingface_hub cache dir among
        // other things) but nothing engine-specific the way llama.cpp's
        // DYLD_LIBRARY_PATH override is — the venv's own `python` binary
        // already has its site-packages resolved relative to itself, and
        // MLX's compiled extension links against the system Metal
        // framework, not a bundled dylib next to the interpreter.
        let source: HashMap<String, String> = std::env::vars().collect();
        let env = EnvPolicy::inherit_filtered();
        cmd.env_clear().envs(env.build(&source));

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            // New process group, leader = this child's own pid — lets a
            // future `killpg`-based hard stop reap any grandchildren too
            // (`python -m mlx_lm.server` doesn't fork additional workers
            // today, but this stays consistent with `LlamaServer`'s own
            // reasoning rather than assuming that never changes).
            cmd.process_group(0);
        }

        let mut child = cmd.spawn()?;
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let log_tail = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

        spawn_drain(stdout, config.log_path.clone(), log_tail.clone());
        spawn_drain(stderr, config.log_path, log_tail.clone());

        Ok(Self {
            child: Mutex::new(Some(child)),
            pid,
            port,
            log_tail,
        })
    }

    fn tail(&self) -> String {
        self.log_tail.lock().expect("log tail mutex").join("\n")
    }

    /// Poll until the child answers ok, exits, or `timeout` elapses.
    /// `probe` is injected (rather than this module dialing HTTP itself)
    /// so `valyria-runtime-openai-compat`'s already-tested transport is
    /// the only thing that ever speaks the wire protocol.
    pub async fn await_ready<F, Fut>(&self, timeout: Duration, mut probe: F) -> Result<()>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = tokio::time::Instant::now() + timeout;
        for delay in Backoff::new(Duration::from_millis(200), Duration::from_secs(2)) {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            if let Some(code) = self.exit_code_if_finished().await {
                return Err(MlxError::Exited {
                    code,
                    log_tail: self.tail(),
                });
            }
            if probe().await {
                return Ok(());
            }
            tokio::time::sleep(
                delay.min(deadline.saturating_duration_since(tokio::time::Instant::now())),
            )
            .await;
        }
        Err(MlxError::NotReady {
            timeout_secs: timeout.as_secs(),
            detail: format!("no successful /health within {} attempts", LOG_TAIL_LINES),
            log_tail: self.tail(),
        })
    }

    async fn exit_code_if_finished(&self) -> Option<Option<i32>> {
        let mut guard = self.child.lock().await;
        let child = guard.as_mut()?;
        match child.try_wait() {
            Ok(Some(status)) => Some(status.code()),
            _ => None,
        }
    }

    /// Idempotent graceful stop: `SIGTERM`, wait up to
    /// [`GRACEFUL_STOP_TIMEOUT`], then `SIGKILL`. A no-op if already
    /// stopped (or never started).
    pub async fn shutdown(&self) {
        let mut guard = self.child.lock().await;
        let Some(mut child) = guard.take() else {
            return; // already shut down
        };
        if let Some(pid) = self.pid {
            terminate(pid);
        }
        let waited = tokio::time::timeout(GRACEFUL_STOP_TIMEOUT, child.wait()).await;
        if waited.is_err() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

fn free_port() -> std::io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    listener.local_addr().map(|a| a.port())
}

#[cfg(unix)]
fn terminate(pid: u32) {
    // SAFETY: `kill` with a pid this struct itself spawned (via
    // `process_group(0)`, so it's also that group's leader) and a plain
    // signal number is a well-defined libc call with no invariants beyond
    // "the pid is valid", which it is here — mirrors
    // `valyria_process::runner::kill_process_group`.
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(not(unix))]
fn terminate(_pid: u32) {
    // MLX is Apple-Silicon-only (macOS), so this branch never actually
    // runs — kept only so the crate still compiles on every platform
    // `cargo check --workspace` covers.
}

fn spawn_drain<R>(
    reader: Option<R>,
    log_path: PathBuf,
    tail: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let Some(reader) = reader else { return };
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .await
            .ok();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(f) = file.as_mut() {
                use tokio::io::AsyncWriteExt;
                let _ = f.write_all(line.as_bytes()).await;
                let _ = f.write_all(b"\n").await;
            }
            let mut t = tail.lock().expect("log tail mutex");
            t.push(line);
            if t.len() > LOG_TAIL_LINES {
                let excess = t.len() - LOG_TAIL_LINES;
                t.drain(0..excess);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_port_returns_a_bindable_loopback_port() {
        let port = free_port().unwrap();
        assert!(port > 0);
        let l2 = TcpListener::bind(("127.0.0.1", port));
        assert!(l2.is_ok());
    }

    #[tokio::test]
    async fn await_ready_reports_exit_when_probe_process_already_finished() {
        // A server that exits immediately (missing venv, bad flags) must
        // be reported as `Exited`, not spun through the full readiness
        // timeout.
        let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "true" });
        if cfg!(windows) {
            cmd.args(["/C", "exit 1"]);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let log_tail = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let dir = tempfile::tempdir().unwrap();
        spawn_drain(stdout, dir.path().join("out.log"), log_tail.clone());
        spawn_drain(stderr, dir.path().join("out.log"), log_tail.clone());
        let server = MlxServer {
            child: Mutex::new(Some(child)),
            pid,
            port: 0,
            log_tail,
        };
        tokio::time::sleep(Duration::from_millis(100)).await;
        let err = server
            .await_ready(Duration::from_secs(5), || async { false })
            .await
            .unwrap_err();
        assert!(matches!(err, MlxError::Exited { .. }));
    }

    #[tokio::test]
    async fn shutdown_is_idempotent() {
        let mut cmd = Command::new(if cfg!(windows) { "cmd" } else { "sleep" });
        if cfg!(windows) {
            cmd.args(["/C", "timeout 30"]);
        } else {
            cmd.arg("30");
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let log_tail = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let dir = tempfile::tempdir().unwrap();
        spawn_drain(stdout, dir.path().join("o.log"), log_tail.clone());
        spawn_drain(stderr, dir.path().join("o.log"), log_tail.clone());
        let server = MlxServer {
            child: Mutex::new(Some(child)),
            pid,
            port: 0,
            log_tail,
        };
        server.shutdown().await;
        server.shutdown().await; // must not panic / hang the second time
    }

    #[tokio::test]
    async fn spawn_with_a_missing_python_surfaces_a_clean_spawn_error() {
        let dir = tempfile::tempdir().unwrap();
        let config = MlxServerConfig {
            python: dir.path().join("no-such-python"),
            model_dir: dir.path().join("model"),
            extra_args: Vec::new(),
            log_path: dir.path().join("logs/mlx.log"),
        };
        let result = MlxServer::spawn(config).await;
        assert!(matches!(result, Err(MlxError::Spawn(_))));
    }
}
