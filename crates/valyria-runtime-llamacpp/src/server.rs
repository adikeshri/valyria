//! `LlamaServer`: spawns and supervises one `llama-server` child process.
//! Not `#![forbid(unsafe_code)]` — a graceful stop needs a raw `SIGTERM`
//! before the hard `SIGKILL` tokio's `kill_on_drop` already gives us,
//! mirroring `valyria_process::runner::kill_process_group`'s justification.

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

use crate::error::{LlamaError, Result};

/// How long a `/health` readiness poll waits before giving up. Generous —
/// a cold 7B model on a slow disk can take the better part of a minute to
/// mmap and warm up.
pub const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(120);
const GRACEFUL_STOP_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_TAIL_LINES: usize = 20;

pub struct LlamaServerConfig {
    pub binary: PathBuf,
    pub weights: PathBuf,
    pub ctx_size: u32,
    /// Extra flags appended verbatim after the standard set.
    pub extra_args: Vec<String>,
    /// Where to append the child's interleaved stdout/stderr.
    pub log_path: PathBuf,
}

/// A running (or exited) `llama-server` child, its assigned loopback port,
/// and a tail buffer of its own log for error messages. `shutdown` is
/// idempotent; if it's never called, `kill_on_drop` on the underlying
/// `tokio::process::Command` still reaps the child when this value drops.
pub struct LlamaServer {
    child: Mutex<Option<Child>>,
    pid: Option<u32>,
    port: u16,
    log_tail: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl LlamaServer {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub async fn spawn(config: LlamaServerConfig) -> Result<Self> {
        let port = free_port().map_err(LlamaError::Spawn)?;
        if let Some(parent) = config.log_path.parent() {
            std::fs::create_dir_all(parent).map_err(LlamaError::Spawn)?;
        }

        let mut cmd = Command::new(&config.binary);
        cmd.arg("-m")
            .arg(&config.weights)
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .arg("-c")
            .arg(config.ctx_size.to_string())
            // Enables the GGUF's own chat template, including tool-call
            // formatting where the model supports it.
            .arg("--jinja")
            .args(&config.extra_args);

        // The release archive ships `llama-server` next to its own
        // .dylib/.so siblings; point the platform loader at that
        // directory so it need not be installed system-wide.
        let lib_dir = config
            .binary
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let source: HashMap<String, String> = std::env::vars().collect();
        let mut env = EnvPolicy::inherit_filtered();
        if cfg!(target_os = "macos") {
            env = env.with_var("DYLD_LIBRARY_PATH", &lib_dir);
        } else if cfg!(target_os = "linux") {
            env = env.with_var("LD_LIBRARY_PATH", &lib_dir);
        }
        cmd.env_clear().envs(env.build(&source));

        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        {
            // New process group, leader = this child's own pid — lets a
            // future `killpg`-based hard stop reap any grandchildren too
            // (mirrors `valyria_process::runner`'s reasoning). The graceful
            // path here only signals the leader pid directly.
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
                return Err(LlamaError::Exited {
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
        Err(LlamaError::NotReady {
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
    // Windows has no SIGTERM equivalent worth reaching for here;
    // `shutdown`'s timeout-then-`start_kill` fallback handles it.
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
        // The port is free again immediately after (the listener was
        // dropped) — bind it a second time to prove that.
        let l2 = TcpListener::bind(("127.0.0.1", port));
        assert!(l2.is_ok());
    }

    #[tokio::test]
    async fn await_ready_reports_exit_when_probe_process_already_finished() {
        // A server that exits immediately (invalid flags, missing model)
        // must be reported as `Exited`, not spun through the full
        // readiness timeout.
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
        let server = LlamaServer {
            child: Mutex::new(Some(child)),
            pid,
            port: 0,
            log_tail,
        };
        // give the child a moment to actually exit
        tokio::time::sleep(Duration::from_millis(100)).await;
        let err = server
            .await_ready(Duration::from_secs(5), || async { false })
            .await
            .unwrap_err();
        assert!(matches!(err, LlamaError::Exited { .. }));
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
        let server = LlamaServer {
            child: Mutex::new(Some(child)),
            pid,
            port: 0,
            log_tail,
        };
        server.shutdown().await;
        server.shutdown().await; // must not panic / hang the second time
    }
}
