//! Running one verification command and turning the result into
//! [`Evidence`] (§27, D4).
//!
//! This crate is the only place a [`VerificationRunId`] is minted, which
//! is what makes [`VerificationRun::evidence`] the sole path to
//! verification-sourced `Evidence`: a model can claim "the tests pass",
//! but the completion report is built from `Evidence` rows, and the only
//! way one of those exists is that a command in here actually ran.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use valyria_process::{EndReason, EnvPolicy};
use valyria_sandbox::{ProcessLauncher, SandboxProfile};
use valyria_types::{Evidence, EvidenceBody, EvidenceSource, Timestamp, VerificationRunId};
use valyria_util::{CancellationToken, Clock, ContentHash};

use crate::command::VerifyCommand;
use crate::error::Result;
use crate::parse::{parse_output, Failure, RawOutput};
use crate::strategy::Tier;

/// How a run ended, at the granularity the strategy and diagnosis care
/// about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationOutcome {
    /// Exit 0.
    Passed,
    /// Ran to completion with a non-zero exit.
    Failed,
    /// Could not be run to a verdict (spawn failure, killed, cancelled).
    Errored,
    /// Exceeded its time budget.
    TimedOut,
}

impl VerificationOutcome {
    pub fn passed(self) -> bool {
        self == VerificationOutcome::Passed
    }
}

/// One executed check. Serializable so it can be journaled and persisted
/// verbatim; [`evidence`](Self::evidence) is the D4 hook.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerificationRun {
    pub id: VerificationRunId,
    pub command: VerifyCommand,
    pub tier: Option<Tier>,
    pub outcome: VerificationOutcome,
    pub exit_code: Option<i32>,
    pub duration_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    /// Hash of the change set this run applied to (§27 "the changeset it
    /// applied to"), when the caller supplied one.
    pub changeset_hash: Option<ContentHash>,
    pub captured_at: Timestamp,
    pub failures: Vec<Failure>,
}

impl VerificationRun {
    pub fn passed(&self) -> bool {
        self.outcome.passed()
    }

    /// The single factual record this run contributes. Body is JSON so
    /// the completion report can pull structured fields (command, outcome,
    /// failure count) without re-parsing text.
    pub fn evidence(&self) -> Evidence {
        let body = serde_json::json!({
            "command": self.command.display(),
            "kind": self.command.kind.as_str(),
            "outcome": self.outcome,
            "exit_code": self.exit_code,
            "duration_ms": self.duration_ms,
            "failure_count": self.failures.len(),
            "failures": self.failures,
        });
        Evidence::new(
            EvidenceSource::Verification(self.id),
            self.captured_at,
            EvidenceBody::Json(body),
        )
    }

    /// A compact, model-facing digest — never the raw output.
    pub fn digest(&self, max_failures: usize) -> String {
        let mut s = format!(
            "{} → {:?} (exit {:?}, {} ms)",
            self.command.display(),
            self.outcome,
            self.exit_code,
            self.duration_ms
        );
        for f in self.failures.iter().take(max_failures) {
            s.push_str("\n  - ");
            s.push_str(&f.message);
            if let Some(loc) = &f.primary_location {
                s.push_str(&format!(
                    " [{}{}]",
                    loc.file.display(),
                    loc.line.map(|l| format!(":{l}")).unwrap_or_default()
                ));
            }
        }
        if self.failures.len() > max_failures {
            s.push_str(&format!(
                "\n  … {} more",
                self.failures.len() - max_failures
            ));
        }
        s
    }
}

/// The behaviour the driver depends on, as a trait so the agent loop can
/// be tested without spawning processes.
#[async_trait]
pub trait Verifier: Send + Sync {
    async fn run(
        &self,
        command: &VerifyCommand,
        tier: Option<Tier>,
        changeset_hash: Option<ContentHash>,
        cancel: CancellationToken,
    ) -> Result<VerificationRun>;
}

/// The real implementation: executes through `valyria-process`, under the
/// workspace's sandbox launcher when one is configured (D10 — parity with
/// the process-executing tools).
pub struct VerificationRunner {
    cwd: PathBuf,
    clock: std::sync::Arc<dyn Clock>,
    timeout: Duration,
    sandbox: Option<(std::sync::Arc<dyn ProcessLauncher>, SandboxProfile)>,
}

impl std::fmt::Debug for VerificationRunner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerificationRunner")
            .field("cwd", &self.cwd)
            .field("timeout", &self.timeout)
            .field("sandboxed", &self.sandbox.is_some())
            .finish()
    }
}

impl VerificationRunner {
    pub fn new(cwd: impl Into<PathBuf>, clock: std::sync::Arc<dyn Clock>) -> Self {
        Self {
            cwd: cwd.into(),
            clock,
            timeout: Duration::from_secs(300),
            sandbox: None,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run every command through `launcher` under `profile` — the same
    /// confinement the process tools use.
    pub fn with_sandbox(
        mut self,
        launcher: std::sync::Arc<dyn ProcessLauncher>,
        profile: SandboxProfile,
    ) -> Self {
        self.sandbox = Some((launcher, profile));
        self
    }

    fn env(&self) -> HashMap<String, String> {
        EnvPolicy::inherit_filtered().build(&std::env::vars().collect())
    }
}

#[async_trait]
impl Verifier for VerificationRunner {
    async fn run(
        &self,
        command: &VerifyCommand,
        tier: Option<Tier>,
        changeset_hash: Option<ContentHash>,
        cancel: CancellationToken,
    ) -> Result<VerificationRun> {
        let spec = command.to_spec(self.cwd.clone(), self.env(), self.timeout);
        let spec =
            match &self.sandbox {
                Some((launcher, profile)) => launcher.wrap(spec, profile).map_err(|e| {
                    valyria_process::ProcessError::Spawn {
                        program: command.program.clone(),
                        source: std::io::Error::other(e.to_string()),
                    }
                })?,
                None => spec,
            };
        let exec = valyria_process::run(&spec, cancel).await?;

        let (outcome, timed_out) = match exec.end_reason {
            EndReason::Exited if exec.exit_code == Some(0) => (VerificationOutcome::Passed, false),
            EndReason::Exited => (VerificationOutcome::Failed, false),
            EndReason::TimedOut | EndReason::IdleTimedOut => (VerificationOutcome::TimedOut, true),
            EndReason::Cancelled => (VerificationOutcome::Errored, false),
        };

        let raw = RawOutput {
            stdout: &exec.stdout.text,
            stderr: &exec.stderr.text,
            exit_code: exec.exit_code,
            timed_out,
        };
        let failures = if outcome == VerificationOutcome::Passed {
            Vec::new()
        } else {
            parse_output(command, &raw)
        };

        Ok(VerificationRun {
            id: VerificationRunId::new(),
            command: command.clone(),
            tier,
            outcome,
            exit_code: exec.exit_code,
            duration_ms: exec.duration.as_millis() as u64,
            stdout: exec.stdout.text,
            stderr: exec.stderr.text,
            truncated: exec.stdout.truncated || exec.stderr.truncated,
            changeset_hash,
            captured_at: self.clock.now(),
            failures,
        })
    }
}

/// Convenience for tests and for `diagnose`: synthesize a run from raw
/// captured output without executing anything.
pub fn run_from_captured(
    command: &VerifyCommand,
    tier: Option<Tier>,
    stdout: impl Into<String>,
    stderr: impl Into<String>,
    exit_code: Option<i32>,
    captured_at: Timestamp,
) -> VerificationRun {
    let stdout = stdout.into();
    let stderr = stderr.into();
    let outcome = match exit_code {
        Some(0) => VerificationOutcome::Passed,
        Some(_) => VerificationOutcome::Failed,
        None => VerificationOutcome::Errored,
    };
    let raw = RawOutput {
        stdout: &stdout,
        stderr: &stderr,
        exit_code,
        timed_out: false,
    };
    let failures = if outcome.passed() {
        Vec::new()
    } else {
        parse_output(command, &raw)
    };
    VerificationRun {
        id: VerificationRunId::new(),
        command: command.clone(),
        tier,
        outcome,
        exit_code,
        duration_ms: 0,
        stdout,
        stderr,
        truncated: false,
        changeset_hash: None,
        captured_at,
        failures,
    }
}

/// Hash a change set's file list into a stable `ContentHash` for
/// `VerificationRun::changeset_hash`.
pub fn changeset_hash(files: &[PathBuf]) -> ContentHash {
    let mut sorted: Vec<String> = files.iter().map(|p| p.display().to_string()).collect();
    sorted.sort();
    ContentHash::of_bytes(sorted.join("\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandKind, CommandSource};
    use std::sync::Arc;
    use valyria_util::SystemClock;

    fn cargo_test() -> VerifyCommand {
        VerifyCommand::new(
            CommandKind::Test,
            "cargo",
            ["test"],
            CommandSource::Manifest {
                file: "Cargo.toml".into(),
            },
        )
    }

    #[test]
    fn evidence_source_is_the_run_id() {
        let run = run_from_captured(
            &cargo_test(),
            Some(Tier::Full),
            "test result: FAILED. 0 passed; 1 failed",
            "",
            Some(101),
            Timestamp::from_millis(5),
        );
        let ev = run.evidence();
        match ev.source() {
            EvidenceSource::Verification(id) => assert_eq!(*id, run.id),
            other => panic!("wrong source: {other:?}"),
        }
    }

    #[test]
    fn passing_run_has_no_failures() {
        let run = run_from_captured(
            &cargo_test(),
            None,
            "test result: ok. 3 passed; 0 failed",
            "",
            Some(0),
            Timestamp::from_millis(1),
        );
        assert!(run.passed());
        assert!(run.failures.is_empty());
    }

    #[test]
    fn failing_run_parses_failures() {
        let run = run_from_captured(
            &cargo_test(),
            None,
            "test tests::x ... FAILED\ntest result: FAILED. 0 passed; 1 failed",
            "",
            Some(101),
            Timestamp::from_millis(1),
        );
        assert_eq!(run.outcome, VerificationOutcome::Failed);
        assert_eq!(run.failures.len(), 1);
        assert_eq!(run.failures[0].failing_test.as_deref(), Some("tests::x"));
    }

    #[test]
    fn digest_is_bounded() {
        let mut run = run_from_captured(
            &cargo_test(),
            None,
            "",
            "boom",
            Some(1),
            Timestamp::from_millis(1),
        );
        run.failures = (0..10)
            .map(|i| Failure::new(crate::parse::FailureKind::TestFailure, format!("f{i}")))
            .collect();
        let d = run.digest(3);
        assert!(d.contains("7 more"));
    }

    #[tokio::test]
    async fn runner_executes_a_real_command() {
        let dir = tempfile::tempdir().unwrap();
        let runner = VerificationRunner::new(dir.path(), Arc::new(SystemClock))
            .with_timeout(Duration::from_secs(10));
        let ok = VerifyCommand::new(
            CommandKind::Test,
            "sh",
            ["-c", "exit 0"],
            CommandSource::Convention,
        );
        let run = runner
            .run(&ok, Some(Tier::Full), None, CancellationToken::new())
            .await
            .unwrap();
        assert!(run.passed());

        let bad = VerifyCommand::new(
            CommandKind::Test,
            "sh",
            ["-c", "echo 'boom: it broke' >&2; exit 3"],
            CommandSource::Convention,
        );
        let run = runner
            .run(&bad, Some(Tier::Full), None, CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(run.outcome, VerificationOutcome::Failed);
        assert_eq!(run.exit_code, Some(3));
        assert!(!run.failures.is_empty());
    }

    #[test]
    fn changeset_hash_is_order_independent() {
        let a = changeset_hash(&[PathBuf::from("b.rs"), PathBuf::from("a.rs")]);
        let b = changeset_hash(&[PathBuf::from("a.rs"), PathBuf::from("b.rs")]);
        assert_eq!(a, b);
    }
}
