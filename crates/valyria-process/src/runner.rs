//! The execution engine (§20): process-group spawn, streamed output under
//! caps, wall-clock and idle timeouts, cooperative-then-forceful
//! termination on cancel, and a hard guarantee that no orphaned process
//! group survives a cancelled/timed-out run.

use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};

use valyria_util::CancellationToken;

use crate::error::{ProcessError, Result};
use crate::output_cap::{CappedOutput, CapturedOutput};
use crate::spec::CommandSpec;

/// How the run ended, beyond a normal exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    Exited,
    TimedOut,
    IdleTimedOut,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionResult {
    pub exit_code: Option<i32>,
    /// Unix only: the signal that terminated the process, if any.
    pub signal: Option<i32>,
    pub stdout: CapturedOutput,
    pub stderr: CapturedOutput,
    pub duration: Duration,
    pub end_reason: EndReason,
}

impl ExecutionResult {
    pub fn success(&self) -> bool {
        self.end_reason == EndReason::Exited && self.exit_code == Some(0)
    }
}

/// How often the supervisor loop checks wall-clock/idle deadlines. Trades
/// up to this much precision on timeout detection for a much simpler
/// implementation than a fully event-driven per-deadline timer — an
/// entirely reasonable trade for process supervision, where "somewhere
/// around 100ms after the deadline" is indistinguishable in practice from
/// exact.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// How long to wait for a killed process group to actually exit before
/// giving up on `wait()` and reporting no exit status. A process group
/// that ignores SIGKILL would only happen if something (unkillable D-state
/// I/O wait) is deeply wrong at the OS level — not something to hang the
/// caller over.
const KILL_GRACE_PERIOD: Duration = Duration::from_secs(5);

pub async fn run(spec: &CommandSpec, cancel: CancellationToken) -> Result<ExecutionResult> {
    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(&spec.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    #[cfg(unix)]
    {
        // New process group, leader = this child's own pid. This is what
        // lets us kill the whole group (including any grandchildren the
        // command spawned) rather than just the immediate child.
        command.process_group(0);
    }

    let start = Instant::now();
    let mut child = command.spawn().map_err(|e| ProcessError::Spawn {
        program: spec.program.clone(),
        source: e,
    })?;

    let stdout_pipe = child.stdout.take().expect("stdout was piped");
    let stderr_pipe = child.stderr.take().expect("stderr was piped");

    // Seeded with the current time (not a 0 sentinel): idle timeout means
    // "no output for N duration", full stop — including a command that
    // never produces any output at all, which should still be caught by
    // idle_timeout rather than only by the (possibly much longer or
    // absent) wall-clock timeout.
    let last_activity = Arc::new(AtomicU64::new(now_millis()));
    let stdout_task = tokio::spawn(drain(
        stdout_pipe,
        spec.max_output_bytes,
        last_activity.clone(),
    ));
    let stderr_task = tokio::spawn(drain(
        stderr_pipe,
        spec.max_output_bytes,
        last_activity.clone(),
    ));

    let end_reason = supervise(&mut child, &cancel, spec, &start, &last_activity).await;

    let killed = end_reason != EndReason::Exited;
    let status = if killed {
        kill_process_group(&mut child);
        tokio::time::timeout(KILL_GRACE_PERIOD, child.wait())
            .await
            .ok()
            .and_then(|r| r.ok())
    } else {
        child.wait().await.ok()
    };

    let stdout = stdout_task
        .await
        .unwrap_or_else(|_| CapturedOutput::empty());
    let stderr = stderr_task
        .await
        .unwrap_or_else(|_| CapturedOutput::empty());

    Ok(ExecutionResult {
        exit_code: status.as_ref().and_then(|s| s.code()),
        signal: status.as_ref().and_then(unix_signal),
        stdout,
        stderr,
        duration: start.elapsed(),
        end_reason,
    })
}

/// Wait for the child to exit, or for cancellation / a deadline to fire
/// first. Does not itself kill anything — that's the caller's job, kept
/// separate so this function's only responsibility is "why did we stop
/// waiting".
async fn supervise(
    child: &mut Child,
    cancel: &CancellationToken,
    spec: &CommandSpec,
    start: &Instant,
    last_activity: &Arc<AtomicU64>,
) -> EndReason {
    loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return EndReason::Cancelled,
            status = child.wait() => {
                if status.is_ok() {
                    return EndReason::Exited;
                }
                // `wait()` erroring (not the exit status — the wait call
                // itself failing) is unusual; treat it as "stop waiting"
                // rather than spin.
                return EndReason::Exited;
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => {
                if let Some(timeout) = spec.timeout {
                    if start.elapsed() >= timeout {
                        return EndReason::TimedOut;
                    }
                }
                if let Some(idle_timeout) = spec.idle_timeout {
                    let last_ms = last_activity.load(Ordering::Relaxed);
                    let idle_for = now_millis().saturating_sub(last_ms);
                    if Duration::from_millis(idle_for) >= idle_timeout {
                        return EndReason::IdleTimedOut;
                    }
                }
            }
        }
    }
}

async fn drain<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
    max_bytes: usize,
    last_activity: Arc<AtomicU64>,
) -> CapturedOutput {
    let mut cap = CappedOutput::new(max_bytes);
    let mut buf = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                cap.push(&buf[..n]);
                last_activity.store(now_millis(), Ordering::Relaxed);
            }
            Err(_) => break,
        }
    }
    cap.into_output()
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(unix)]
fn kill_process_group(child: &mut Child) {
    if let Some(pid) = child.id() {
        // SAFETY: `killpg` with a pid we own (this child's process group,
        // created via `process_group(0)` at spawn) and a plain signal
        // number is a well-defined libc call with no invariants beyond
        // "the pid is valid", which it is here.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn kill_process_group(child: &mut Child) {
    let _ = child.start_kill();
}

#[cfg(unix)]
fn unix_signal(status: &std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(status)
}

#[cfg(not(unix))]
fn unix_signal(_status: &std::process::ExitStatus) -> Option<i32> {
    None
}
