//! `valyria-verify` — layer 3 (Execution).
//!
//! Verification, diagnosis and the raw material for repair (§4.26): find
//! the commands a repository actually uses, run them under a cost/value
//! escalation strategy, capture every run as [`Evidence`](valyria_types::Evidence),
//! and distil a failing run into a small structured [`Diagnosis`] rather
//! than a wall of output.
//!
//! - [`discovery`] scans for build/test/lint/format commands and confirms
//!   each by executing a cheap probe.
//! - [`strategy`] orders the confirmed commands into a [`VerificationPlan`]
//!   — syntax first, a mandatory broad run before completion, early exit
//!   on failure.
//! - [`run`] executes one command via `valyria-process`, classifies the
//!   outcome, parses failures, and mints the [`VerificationRunId`](valyria_types::VerificationRunId)
//!   that lets the result become verification-sourced `Evidence` (D4).
//! - [`parse`] holds the per-tool failure parsers (cargo, libtest,
//!   pytest, go test, jest, tsc, mypy, eslint, formatters, generic).
//! - [`diagnose`] intersects failure locations with the change ledger and
//!   the graph neighbourhood to name suspect files.
//! - [`evidence`] persists runs to `workspace.db` (migration block
//!   700-799); [`report`] builds a completion report from those rows and
//!   nothing else.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod command;
pub mod diagnose;
pub mod discovery;
pub mod error;
pub mod evidence;
pub mod parse;
pub mod report;
pub mod run;
pub mod strategy;

pub use command::{CommandKind, CommandSource, VerifyCommand};
pub use diagnose::{diagnose, Diagnosis, Suspect, SuspectReason};
pub use discovery::{
    scan, validate, DiscoveryReport, ProbeOutcome, ProbeRunner, ProcessProbeRunner,
    ValidatedTooling,
};
pub use error::{Result, VerifyError};
pub use evidence::{VerificationLog, VerificationRunRecord, MIGRATIONS};
pub use parse::{parse_output, Assertion, Failure, FailureKind, Location, RawOutput};
pub use report::{CompletionReport, ReportStatus};
pub use run::{
    changeset_hash, run_from_captured, VerificationOutcome, VerificationRun, VerificationRunner,
    Verifier,
};
pub use strategy::{
    ChangeSet, EscalationStrategy, TargetedCheck, Tier, VerificationPlan, VerificationStep,
};
