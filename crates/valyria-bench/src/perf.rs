//! A small, honest performance-budget harness.
//!
//! §9 lists nine performance budgets. The ones that need a 100k-file real
//! repository (cold index, incremental p95, lexical/regex search at
//! scale) wait on this crate's future pinned-repo corpus and a criterion
//! setup — they are **not** claimed here. What this module does provide is
//! a repeatable check of the budgets that need only a runtime and a
//! fixture: task open / resume latency and per-task orchestration
//! overhead. It is wired as an `#[ignore]`d test (`cargo test -p
//! valyria-bench -- --ignored`) so a noisy CI runner never fails a build
//! on a timing wobble, but a regression is one command away.

use std::time::{Duration, Instant};

/// One named budget and whether the measured time met it.
#[derive(Debug, Clone)]
pub struct PerfSample {
    pub name: &'static str,
    pub measured: Duration,
    pub budget: Duration,
}

impl PerfSample {
    pub fn met(&self) -> bool {
        self.measured <= self.budget
    }

    pub fn render(&self) -> String {
        format!(
            "  [{}] {:<28} {:>8.1}ms  (budget {:.0}ms)",
            if self.met() { "ok" } else { "SLOW" },
            self.name,
            self.measured.as_secs_f64() * 1000.0,
            self.budget.as_secs_f64() * 1000.0,
        )
    }
}

/// Time `f`, returning a [`PerfSample`] against `budget`.
pub fn measure<F: FnOnce()>(name: &'static str, budget: Duration, f: F) -> PerfSample {
    let start = Instant::now();
    f();
    PerfSample {
        name,
        measured: start.elapsed(),
        budget,
    }
}

/// §9 budgets covered by this module.
pub mod budget {
    use std::time::Duration;
    /// "Task resume after restart < 2 s".
    pub const TASK_RESUME: Duration = Duration::from_secs(2);
    /// "Tool call overhead (excl. work) < 5 ms" — measured here as the
    /// per-graded-task harness overhead amortised, a looser proxy.
    pub const TASK_OVERHEAD: Duration = Duration::from_millis(250);
}
