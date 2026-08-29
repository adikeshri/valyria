//! `valyria-bench` — layer 6 (Interface).
//!
//! The evaluation harness (§4.30, PLAN Phase 11). A benchmark task is
//! `{ repo, objective, setup, oracle }` where the **oracle is an
//! executable check**, not a human judgement: "these tests pass", "this
//! file now contains X", "no more than N files changed", "these paths were
//! left untouched". A [`suite::fixture_suite`] of such tasks runs fully
//! offline against the deterministic fake model (D12) and is the
//! regression guard for the whole orchestration stack — the same role the
//! nightly fake-model suite plays in §7.
//!
//! What this crate deliberately does **not** do yet: drive a *real* local
//! model (needs a running `llama-server` — the documented Phase 9/10
//! follow-up), or expose `bench.run` over the protocol (kept out of
//! `valyria-cli` so D11's "the CLI cannot grow orchestration" stays
//! literally true; the harness runs from `cargo run -p valyria-bench` and
//! `cargo xtask bench`). SWE-bench-style external suites are an adapter
//! left for later.

#![forbid(unsafe_code)]

pub mod error;
pub mod metrics;
pub mod oracle;
pub mod perf;
pub mod report;
pub mod runner;
pub mod suite;
pub mod task;

pub use error::{BenchError, Result};
pub use metrics::BenchMetrics;
pub use oracle::{Oracle, OracleContext, OracleVerdict};
pub use report::{compare, BenchReport, Comparison, MetricDelta};
pub use runner::{BenchOutcome, BenchRunner};
pub use suite::fixture_suite;
pub use task::{BenchTask, RepoSpec, TaskCategory};

/// The crate's build-plan phase, kept for the layering/CI checks that
/// predate the implementation.
pub const PHASE: u8 = 11;
