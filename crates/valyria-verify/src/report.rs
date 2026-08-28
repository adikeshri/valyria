//! The completion report (§15, D4): what the runtime actually verified,
//! assembled **only** from persisted [`VerificationRunRecord`]s.
//!
//! If the model said "all tests pass" and there is no passing test-tier
//! run in the log, the report says *not verified* — the claim never
//! becomes a fact just by being asserted.

use serde::{Deserialize, Serialize};
use valyria_types::TaskId;

use crate::evidence::VerificationRunRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    /// A broad (test-tier) run passed and nothing in the log is failing.
    Verified,
    /// Some checks passed but no broad test run did, or style checks were
    /// skipped.
    PartiallyVerified,
    /// Nothing was run, or the log has no passing run at all.
    NotVerified,
    /// The latest run for one or more kinds is failing.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VerifiedClaim {
    pub kind: String,
    pub command: String,
    pub outcome: String,
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionReport {
    pub task_id: TaskId,
    pub status: ReportStatus,
    /// Facts, each backed by a run id.
    pub verified: Vec<VerifiedClaim>,
    /// Claims that could not be substantiated from the log.
    pub unverified: Vec<String>,
}

impl CompletionReport {
    /// Build from the task's verification log. `model_claims` is the set
    /// of things the agent asserted in its finish message (free text,
    /// lowercased keywords like `"tests pass"`, `"builds"`, `"lint
    /// clean"`); each is checked against the log and demoted to
    /// `unverified` if unsupported.
    pub fn from_runs(
        task_id: TaskId,
        runs: &[VerificationRunRecord],
        model_claims: &[String],
    ) -> Self {
        let mut verified = Vec::new();
        let mut unverified = Vec::new();

        // Latest run per command kind.
        let mut latest: std::collections::BTreeMap<&str, &VerificationRunRecord> =
            std::collections::BTreeMap::new();
        for r in runs {
            latest
                .entry(r.command_kind.as_str())
                .and_modify(|cur| {
                    if r.seq > cur.seq {
                        *cur = r;
                    }
                })
                .or_insert(r);
        }

        let mut any_failed = false;
        let mut test_passed = false;
        for (kind, r) in &latest {
            if r.passed() {
                verified.push(VerifiedClaim {
                    kind: (*kind).to_string(),
                    command: r.command_display.clone(),
                    outcome: "passed".into(),
                    run_id: r.id.to_string(),
                });
                if *kind == "test" {
                    test_passed = true;
                }
            } else {
                any_failed = true;
                unverified.push(format!(
                    "`{}` ({}) last run: {:?}",
                    r.command_display, kind, r.outcome
                ));
            }
        }

        for claim in model_claims {
            let c = claim.to_ascii_lowercase();
            let supported = if c.contains("test") {
                test_passed
            } else if c.contains("build") || c.contains("compile") {
                latest.get("build").map(|r| r.passed()).unwrap_or(false) || test_passed
            } else if c.contains("lint") || c.contains("clippy") {
                latest.get("lint").map(|r| r.passed()).unwrap_or(false)
            } else if c.contains("format") || c.contains("fmt") {
                latest.get("format").map(|r| r.passed()).unwrap_or(false)
            } else {
                false
            };
            if !supported {
                unverified.push(format!("model claimed \"{claim}\" — no supporting run"));
            }
        }

        let status = if runs.is_empty() {
            ReportStatus::NotVerified
        } else if any_failed {
            ReportStatus::Failed
        } else if test_passed {
            ReportStatus::Verified
        } else if !verified.is_empty() {
            ReportStatus::PartiallyVerified
        } else {
            ReportStatus::NotVerified
        };

        Self {
            task_id,
            status,
            verified,
            unverified,
        }
    }

    pub fn render(&self) -> String {
        let mut s = format!("verification: {:?}\n", self.status);
        for v in &self.verified {
            s.push_str(&format!("  ✓ {} — {} [{}]\n", v.kind, v.command, v.run_id));
        }
        for u in &self.unverified {
            s.push_str(&format!("  ✗ {u}\n"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::{CommandKind, CommandSource, VerifyCommand};
    use crate::run::run_from_captured;
    use crate::strategy::Tier;
    use valyria_types::Timestamp;

    fn record(kind: CommandKind, code: i32, seq: u64) -> VerificationRunRecord {
        let cmd = VerifyCommand::new(
            kind,
            "cargo",
            [kind.as_str()],
            CommandSource::Manifest {
                file: "Cargo.toml".into(),
            },
        );
        let run = run_from_captured(
            &cmd,
            Some(Tier::Full),
            if code == 0 { "ok" } else { "boom failed" },
            "",
            Some(code),
            Timestamp::from_millis(1),
        );
        VerificationRunRecord {
            id: run.id,
            task_id: TaskId::new(),
            command_kind: kind.as_str().to_string(),
            command_display: cmd.display(),
            tier: Some("Full".into()),
            outcome: run.outcome,
            exit_code: Some(code),
            duration_ms: 0,
            changeset_hash: None,
            failures: run.failures,
            stdout: run.stdout,
            stderr: run.stderr,
            truncated: false,
            captured_at: Timestamp::from_millis(1),
            seq,
        }
    }

    #[test]
    fn no_runs_is_not_verified() {
        let r = CompletionReport::from_runs(TaskId::new(), &[], &[]);
        assert_eq!(r.status, ReportStatus::NotVerified);
    }

    #[test]
    fn passing_test_run_is_verified() {
        let runs = vec![
            record(CommandKind::Build, 0, 1),
            record(CommandKind::Test, 0, 2),
        ];
        let r = CompletionReport::from_runs(TaskId::new(), &runs, &[]);
        assert_eq!(r.status, ReportStatus::Verified);
        assert_eq!(r.verified.len(), 2);
    }

    #[test]
    fn failing_latest_run_is_failed_even_after_an_earlier_pass() {
        let runs = vec![
            record(CommandKind::Test, 0, 1),
            record(CommandKind::Test, 1, 2),
        ];
        let r = CompletionReport::from_runs(TaskId::new(), &runs, &[]);
        assert_eq!(r.status, ReportStatus::Failed);
    }

    #[test]
    fn unsupported_model_claim_is_demoted() {
        let runs = vec![record(CommandKind::Build, 0, 1)];
        let r = CompletionReport::from_runs(TaskId::new(), &runs, &["tests pass".to_string()]);
        assert!(r.unverified.iter().any(|u| u.contains("no supporting run")));
        assert_eq!(r.status, ReportStatus::PartiallyVerified);
    }

    #[test]
    fn only_style_runs_is_partial() {
        let runs = vec![record(CommandKind::Lint, 0, 1)];
        let r = CompletionReport::from_runs(TaskId::new(), &runs, &[]);
        assert_eq!(r.status, ReportStatus::PartiallyVerified);
    }
}
