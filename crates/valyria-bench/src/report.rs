//! The benchmark report — a serializable, diffable artifact — and
//! `compare(baseline, current)`, which is what `cargo xtask bench`
//! (and, one day, `valyria benchmark --compare`) uses to fail CI on a
//! regression.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::runner::BenchOutcome;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchReport {
    /// Milliseconds since the Unix epoch; informational only, never part
    /// of a comparison.
    pub generated_at_ms: u128,
    pub runs: Vec<BenchOutcome>,
}

impl BenchReport {
    pub fn new(runs: Vec<BenchOutcome>) -> Self {
        Self {
            generated_at_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or(0),
            runs,
        }
    }

    pub fn total(&self) -> usize {
        self.runs.len()
    }

    pub fn passed(&self) -> usize {
        self.runs.iter().filter(|r| r.passed).count()
    }

    pub fn pass_rate(&self) -> f64 {
        if self.runs.is_empty() {
            return 0.0;
        }
        self.passed() as f64 / self.total() as f64
    }

    pub fn all_passed(&self) -> bool {
        !self.runs.is_empty() && self.runs.iter().all(|r| r.passed)
    }

    /// Pass counts grouped by task category.
    pub fn by_category(&self) -> BTreeMap<&'static str, (usize, usize)> {
        let mut out: BTreeMap<&'static str, (usize, usize)> = BTreeMap::new();
        for r in &self.runs {
            let e = out.entry(r.category.as_str()).or_default();
            e.1 += 1;
            if r.passed {
                e.0 += 1;
            }
        }
        out
    }

    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("BenchReport serializes")
    }

    pub fn from_json(s: &str) -> serde_json::Result<Self> {
        serde_json::from_str(s)
    }

    /// A copy with every wall-clock field zeroed, for use as a committed
    /// baseline artifact: `compare` ignores timing, so this keeps the
    /// checked-in file byte-stable across runs while staying a real
    /// pass/fail + cost-metric record.
    pub fn stabilized(&self) -> Self {
        let mut c = self.clone();
        c.generated_at_ms = 0;
        for r in &mut c.runs {
            r.metrics.wall_ms = 0;
        }
        c
    }

    /// A compact human table for the CLI / xtask output.
    pub fn render_table(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "bench: {}/{} passed ({:.0}%)\n",
            self.passed(),
            self.total(),
            self.pass_rate() * 100.0
        ));
        for (cat, (pass, total)) in self.by_category() {
            out.push_str(&format!("  {cat:<16} {pass}/{total}\n"));
        }
        out.push('\n');
        for r in &self.runs {
            out.push_str(&format!(
                "  [{}] {:<24} {:<10} {}\n",
                if r.passed { "ok" } else { "FAIL" },
                r.id,
                r.final_state,
                r.oracle_detail
            ));
        }
        out
    }
}

// --- comparison -----------------------------------------------------

/// A cost-metric change on a task that passed in both runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetricDelta {
    pub task: String,
    pub metric: String,
    pub baseline: u64,
    pub current: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Comparison {
    /// Tasks that passed in the baseline and now fail, or are newly
    /// missing from the current run.
    pub regressions: Vec<String>,
    /// Tasks that failed in the baseline and now pass, or are new.
    pub improvements: Vec<String>,
    /// Tasks whose pass/fail is unchanged.
    pub unchanged: usize,
    /// Cost blow-ups worth a look (more than `tolerance` extra, and at
    /// least 50% more) on a task that still passes.
    pub metric_regressions: Vec<MetricDelta>,
}

impl Comparison {
    /// A comparison is "clean" (CI-passing) iff nothing regressed.
    pub fn is_clean(&self) -> bool {
        self.regressions.is_empty() && self.metric_regressions.is_empty()
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.is_clean() {
            out.push_str("comparison: clean — no regressions\n");
        } else {
            out.push_str("comparison: REGRESSIONS\n");
        }
        for r in &self.regressions {
            out.push_str(&format!("  regressed: {r}\n"));
        }
        for d in &self.metric_regressions {
            out.push_str(&format!(
                "  cost up:   {} {} {} -> {}\n",
                d.task, d.metric, d.baseline, d.current
            ));
        }
        for i in &self.improvements {
            out.push_str(&format!("  improved:  {i}\n"));
        }
        out.push_str(&format!("  unchanged: {}\n", self.unchanged));
        out
    }
}

/// Diff two reports. `tolerance` is the absolute slack allowed on a cost
/// metric before it counts as a regression (it must *also* be a ≥50%
/// jump, so tiny counts near zero don't trip it).
pub fn compare_with_tolerance(
    baseline: &BenchReport,
    current: &BenchReport,
    tolerance: u64,
) -> Comparison {
    let base: BTreeMap<&str, &BenchOutcome> =
        baseline.runs.iter().map(|r| (r.id.as_str(), r)).collect();
    let cur: BTreeMap<&str, &BenchOutcome> =
        current.runs.iter().map(|r| (r.id.as_str(), r)).collect();

    let mut cmp = Comparison::default();

    for (id, b) in &base {
        match cur.get(id) {
            None => {
                if b.passed {
                    cmp.regressions
                        .push(format!("{id} (missing from current run)"));
                }
            }
            Some(c) => {
                if b.passed && !c.passed {
                    cmp.regressions.push((*id).to_string());
                } else if !b.passed && c.passed {
                    cmp.improvements.push((*id).to_string());
                } else {
                    cmp.unchanged += 1;
                }
                if b.passed && c.passed {
                    let bm: BTreeMap<_, _> = b.metrics.cost_fields().into_iter().collect();
                    for (metric, cv) in c.metrics.cost_fields() {
                        let bv = bm.get(metric).copied().unwrap_or(0);
                        if cv > bv + tolerance && cv * 2 >= bv * 3 {
                            cmp.metric_regressions.push(MetricDelta {
                                task: (*id).to_string(),
                                metric: metric.to_string(),
                                baseline: bv,
                                current: cv,
                            });
                        }
                    }
                }
            }
        }
    }
    for id in cur.keys() {
        if !base.contains_key(id) {
            cmp.improvements.push(format!("{id} (new)"));
        }
    }
    cmp
}

/// [`compare_with_tolerance`] with a default slack of 2.
pub fn compare(baseline: &BenchReport, current: &BenchReport) -> Comparison {
    compare_with_tolerance(baseline, current, 2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metrics::BenchMetrics;
    use crate::task::TaskCategory;

    fn outcome(id: &str, passed: bool, model_calls: u32) -> BenchOutcome {
        BenchOutcome {
            id: id.into(),
            category: TaskCategory::Feature,
            objective: "x".into(),
            final_state: "COMPLETED".into(),
            report_status: "NotVerified".into(),
            oracle: "o".into(),
            oracle_detail: "d".into(),
            passed,
            metrics: BenchMetrics {
                wall_ms: 0,
                state_changes: 0,
                model_calls,
                tool_calls: 0,
                files_changed: 0,
                verification_runs: 0,
                tests_passed: 0,
                tests_failed: 0,
                progress_stalls: 0,
                reached_terminal: true,
            },
        }
    }

    #[test]
    fn identical_reports_compare_clean() {
        let r = BenchReport::new(vec![outcome("a", true, 3), outcome("b", true, 2)]);
        let cmp = compare(&r, &r);
        assert!(cmp.is_clean());
        assert_eq!(cmp.unchanged, 2);
    }

    #[test]
    fn a_now_failing_task_is_a_regression() {
        let base = BenchReport::new(vec![outcome("a", true, 3)]);
        let cur = BenchReport::new(vec![outcome("a", false, 3)]);
        let cmp = compare(&base, &cur);
        assert!(!cmp.is_clean());
        assert_eq!(cmp.regressions, vec!["a".to_string()]);
    }

    #[test]
    fn a_newly_passing_task_is_an_improvement_not_a_regression() {
        let base = BenchReport::new(vec![outcome("a", false, 3)]);
        let cur = BenchReport::new(vec![outcome("a", true, 3)]);
        let cmp = compare(&base, &cur);
        assert!(cmp.is_clean());
        assert_eq!(cmp.improvements, vec!["a".to_string()]);
    }

    #[test]
    fn a_big_cost_jump_on_a_passing_task_is_a_metric_regression() {
        let base = BenchReport::new(vec![outcome("a", true, 3)]);
        let cur = BenchReport::new(vec![outcome("a", true, 9)]);
        let cmp = compare(&base, &cur);
        assert!(!cmp.is_clean());
        assert_eq!(cmp.metric_regressions.len(), 1);
        assert_eq!(cmp.metric_regressions[0].metric, "model_calls");
    }

    #[test]
    fn a_small_cost_wobble_is_tolerated() {
        let base = BenchReport::new(vec![outcome("a", true, 3)]);
        let cur = BenchReport::new(vec![outcome("a", true, 4)]);
        assert!(compare(&base, &cur).is_clean());
    }

    #[test]
    fn missing_previously_passing_task_is_a_regression() {
        let base = BenchReport::new(vec![outcome("a", true, 3), outcome("b", true, 3)]);
        let cur = BenchReport::new(vec![outcome("a", true, 3)]);
        let cmp = compare(&base, &cur);
        assert!(cmp.regressions.iter().any(|r| r.starts_with("b")));
    }

    #[test]
    fn report_json_round_trips() {
        let r = BenchReport::new(vec![outcome("a", true, 3)]);
        let back = BenchReport::from_json(&r.to_json_pretty()).unwrap();
        assert_eq!(back.total(), 1);
        assert!(back.all_passed());
    }

    #[test]
    fn stabilized_zeroes_timing_only() {
        let mut r = BenchReport::new(vec![outcome("a", true, 3)]);
        r.runs[0].metrics.wall_ms = 999;
        let s = r.stabilized();
        assert_eq!(s.generated_at_ms, 0);
        assert_eq!(s.runs[0].metrics.wall_ms, 0);
        assert_eq!(s.runs[0].metrics.model_calls, 3);
    }
}
