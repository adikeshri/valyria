//! Loop and progress detection (§31): a stalled agent must never spin
//! silently. This module is pure bookkeeping — the driver feeds it a
//! [`StepSignature`] after every step, a failure fingerprint after every
//! verification, and a [`ProgressMetric`] snapshot, and it answers "are we
//! going in circles" with a typed [`LoopFinding`].
//!
//! Five detector classes, each with its own trigger:
//!
//! | class | fires when |
//! |---|---|
//! | [`LoopFinding::ExactRepeat`] | the identical step signature recurs |
//! | [`LoopFinding::Oscillation`] | signatures cycle `A → B → A → B` |
//! | [`LoopFinding::RepeatedFailure`] | the same failure fingerprint N times |
//! | [`LoopFinding::NoChangeIteration`] | steps run but the file state hash never moves |
//! | [`LoopFinding::FrontierStalled`] | the verification frontier stops advancing |

use std::collections::BTreeSet;
use std::path::PathBuf;

use valyria_util::ContentHash;

/// A fingerprint of one agent step, across every axis a loop could hide
/// in. All fields optional: a pure-reasoning step has no patch, a step
/// that ran no tool has no tool call, etc.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StepSignature {
    /// Normalized `tool(canonical-json-args)`.
    pub tool_call: Option<String>,
    /// Hash of the edit's resulting content / patch text.
    pub patch_hash: Option<ContentHash>,
    /// Fingerprint of the failure this step was reacting to.
    pub error_fingerprint: Option<String>,
    /// Hash of the workspace file state after this step.
    pub file_state_hash: Option<ContentHash>,
    /// Hash of the retrieved-context set used for this step.
    pub context_hash: Option<ContentHash>,
}

impl StepSignature {
    pub fn with_tool_call(mut self, tool: &str, input: &serde_json::Value) -> Self {
        self.tool_call = Some(normalized_tool_call(tool, input));
        self
    }

    pub fn with_patch(mut self, new_content: &str) -> Self {
        self.patch_hash = Some(ContentHash::of_bytes(new_content.as_bytes()));
        self
    }

    pub fn with_error(mut self, fingerprint: impl Into<String>) -> Self {
        self.error_fingerprint = Some(fingerprint.into());
        self
    }

    pub fn with_file_state(mut self, hash: ContentHash) -> Self {
        self.file_state_hash = Some(hash);
        self
    }

    pub fn with_context(mut self, hash: ContentHash) -> Self {
        self.context_hash = Some(hash);
        self
    }

    /// The identity used for "same step again" comparisons — everything
    /// except the context hash (the same action taken with slightly
    /// different context in view is still the same action).
    fn key(&self) -> String {
        format!(
            "{}|{}|{}|{}",
            self.tool_call.as_deref().unwrap_or("-"),
            self.patch_hash.map(|h| h.to_hex()).unwrap_or_default(),
            self.error_fingerprint.as_deref().unwrap_or("-"),
            self.file_state_hash.map(|h| h.to_hex()).unwrap_or_default(),
        )
    }
}

/// `tool({"a":1,"b":2})` with object keys sorted, so argument ordering
/// noise does not hide a repeat.
pub fn normalized_tool_call(tool: &str, input: &serde_json::Value) -> String {
    fn canon(v: &serde_json::Value) -> serde_json::Value {
        match v {
            serde_json::Value::Object(map) => {
                let mut sorted: Vec<(&String, &serde_json::Value)> = map.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(b.0));
                serde_json::Value::Object(
                    sorted
                        .into_iter()
                        .map(|(k, v)| (k.clone(), canon(v)))
                        .collect(),
                )
            }
            serde_json::Value::Array(items) => {
                serde_json::Value::Array(items.iter().map(canon).collect())
            }
            other => other.clone(),
        }
    }
    format!("{tool}({})", canon(input))
}

/// A snapshot of "how far along" the agent is, for the progress metric.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgressMetric {
    /// Index of the furthest verification tier reached (see
    /// `valyria_verify::strategy::Tier`).
    pub verification_frontier: usize,
    /// How many distinct failures the latest run reported.
    pub failure_count: usize,
    /// Every file the agent has written so far this task.
    pub files_touched: BTreeSet<PathBuf>,
}

impl ProgressMetric {
    /// Real progress = the frontier advanced, or fewer failures, or a
    /// previously-untouched file was edited.
    pub fn advanced_since(&self, prev: &ProgressMetric) -> bool {
        self.verification_frontier > prev.verification_frontier
            || self.failure_count < prev.failure_count
            || self.files_touched.len() > prev.files_touched.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopFinding {
    /// The same step (tool call + patch + error + file state) ran
    /// `count` times.
    ExactRepeat { key: String, count: usize },
    /// The last `2 * period` steps alternate between two signatures.
    Oscillation { period: usize },
    /// The identical failure fingerprint came back `count` times in a
    /// row.
    RepeatedFailure { fingerprint: String, count: usize },
    /// `iterations` steps ran without the workspace file state changing.
    NoChangeIteration { iterations: usize },
    /// `iterations` verification cycles with no progress-metric advance.
    FrontierStalled { iterations: usize },
}

impl LoopFinding {
    pub fn code(&self) -> &'static str {
        match self {
            LoopFinding::ExactRepeat { .. } => "exact_repeat",
            LoopFinding::Oscillation { .. } => "oscillation",
            LoopFinding::RepeatedFailure { .. } => "repeated_failure",
            LoopFinding::NoChangeIteration { .. } => "no_change_iteration",
            LoopFinding::FrontierStalled { .. } => "frontier_stalled",
        }
    }
}

/// Thresholds for the detectors. Defaults are deliberately small — a
/// stalled agent is expensive, and every finding routes to *escalation*,
/// not to failure, so a false positive costs one strategy change.
#[derive(Debug, Clone)]
pub struct DetectorConfig {
    /// Fire `ExactRepeat` once a signature has been seen this many times.
    pub exact_repeat_at: usize,
    /// Longest cycle period checked for `Oscillation`.
    pub max_oscillation_period: usize,
    /// Fire `RepeatedFailure` after this many identical fingerprints.
    pub repeated_failure_at: usize,
    /// Fire `NoChangeIteration` / `FrontierStalled` after this many
    /// no-advance iterations.
    pub stall_at: usize,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            exact_repeat_at: 3,
            max_oscillation_period: 3,
            repeated_failure_at: 3,
            stall_at: 3,
        }
    }
}

/// Accumulates step / failure / progress observations for one task run.
#[derive(Debug, Clone)]
pub struct LoopDetector {
    config: DetectorConfig,
    steps: Vec<StepSignature>,
    failures: Vec<String>,
    progress: Vec<ProgressMetric>,
    no_change_run: usize,
    stalled_run: usize,
}

impl Default for LoopDetector {
    fn default() -> Self {
        Self::new(DetectorConfig::default())
    }
}

impl LoopDetector {
    pub fn new(config: DetectorConfig) -> Self {
        Self {
            config,
            steps: Vec::new(),
            failures: Vec::new(),
            progress: Vec::new(),
            no_change_run: 0,
            stalled_run: 0,
        }
    }

    /// Record one completed step. Returns the first loop class it trips.
    pub fn observe_step(&mut self, sig: StepSignature) -> Option<LoopFinding> {
        // No-change detection: consecutive steps with a file_state_hash
        // that equals the previous one.
        if let (Some(prev), Some(cur)) = (
            self.steps.last().and_then(|s| s.file_state_hash),
            sig.file_state_hash,
        ) {
            if prev == cur {
                self.no_change_run += 1;
            } else {
                self.no_change_run = 0;
            }
        } else {
            self.no_change_run = 0;
        }

        self.steps.push(sig);

        if self.no_change_run + 1 >= self.config.stall_at
            && self
                .steps
                .iter()
                .rev()
                .take(self.no_change_run + 1)
                .any(|s| s.tool_call.is_some() || s.patch_hash.is_some())
        {
            return Some(LoopFinding::NoChangeIteration {
                iterations: self.no_change_run + 1,
            });
        }

        if let Some(f) = self.detect_exact_repeat() {
            return Some(f);
        }
        self.detect_oscillation()
    }

    /// Record the fingerprint of the failure a verification run produced
    /// (or `None` for a passing run, which clears the streak).
    pub fn observe_failure(&mut self, fingerprint: Option<&str>) -> Option<LoopFinding> {
        match fingerprint {
            None => {
                self.failures.clear();
                None
            }
            Some(fp) => {
                self.failures.push(fp.to_string());
                let n = self
                    .failures
                    .iter()
                    .rev()
                    .take_while(|f| f.as_str() == fp)
                    .count();
                if n >= self.config.repeated_failure_at {
                    Some(LoopFinding::RepeatedFailure {
                        fingerprint: fp.to_string(),
                        count: n,
                    })
                } else {
                    None
                }
            }
        }
    }

    /// Record a progress snapshot after a verification cycle.
    pub fn observe_progress(&mut self, metric: ProgressMetric) -> Option<LoopFinding> {
        if let Some(prev) = self.progress.last() {
            if metric.advanced_since(prev) {
                self.stalled_run = 0;
            } else {
                self.stalled_run += 1;
            }
        }
        self.progress.push(metric);
        if self.stalled_run >= self.config.stall_at {
            Some(LoopFinding::FrontierStalled {
                iterations: self.stalled_run,
            })
        } else {
            None
        }
    }

    fn detect_exact_repeat(&self) -> Option<LoopFinding> {
        let last = self.steps.last()?;
        // A step with nothing distinctive (no tool, no patch, no error)
        // is not worth flagging as a repeat.
        if last.tool_call.is_none() && last.patch_hash.is_none() && last.error_fingerprint.is_none()
        {
            return None;
        }
        let key = last.key();
        // Consecutive identical steps — "the agent did the exact same
        // thing again just now". A signature that recurs but with other
        // steps in between is oscillation's department, not this one.
        let run = self
            .steps
            .iter()
            .rev()
            .take_while(|s| s.key() == key)
            .count();
        if run >= self.config.exact_repeat_at {
            Some(LoopFinding::ExactRepeat { key, count: run })
        } else {
            None
        }
    }

    fn detect_oscillation(&self) -> Option<LoopFinding> {
        let keys: Vec<String> = self.steps.iter().map(|s| s.key()).collect();
        for period in 1..=self.config.max_oscillation_period {
            let need = period * 3; // at least three repeats of the cycle
            if keys.len() < need {
                continue;
            }
            let tail = &keys[keys.len() - need..];
            let cycle = &tail[..period];
            // Not a "cycle" if every element is identical — that's the
            // exact-repeat detector's job.
            if cycle.iter().all(|k| k == &cycle[0]) {
                continue;
            }
            if tail.chunks(period).all(|chunk| chunk == cycle) {
                return Some(LoopFinding::Oscillation { period });
            }
        }
        None
    }

    pub fn step_count(&self) -> usize {
        self.steps.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool_sig(name: &str) -> StepSignature {
        StepSignature::default().with_tool_call(name, &serde_json::json!({"x": 1}))
    }

    #[test]
    fn normalized_tool_call_is_key_order_stable() {
        let a = normalized_tool_call("edit", &serde_json::json!({"a": 1, "b": 2}));
        let b = normalized_tool_call("edit", &serde_json::json!({"b": 2, "a": 1}));
        assert_eq!(a, b);
    }

    #[test]
    fn exact_repeat_fires_on_the_third_identical_step() {
        let mut d = LoopDetector::default();
        assert!(d.observe_step(tool_sig("read_file")).is_none());
        assert!(d.observe_step(tool_sig("read_file")).is_none());
        let finding = d.observe_step(tool_sig("read_file")).unwrap();
        assert!(matches!(finding, LoopFinding::ExactRepeat { count: 3, .. }));
        assert_eq!(finding.code(), "exact_repeat");
    }

    #[test]
    fn oscillation_fires_on_a_b_a_b_a_b() {
        let mut d = LoopDetector::default();
        let seq = ["a", "b", "a", "b", "a", "b"];
        let mut found = None;
        for s in seq {
            found = d.observe_step(tool_sig(s)).or(found);
        }
        // the repeating unit `[a, b]` has length 2
        assert!(matches!(
            found,
            Some(LoopFinding::Oscillation { period: 2 })
        ));
    }

    #[test]
    fn oscillation_period_three() {
        let mut d = LoopDetector::default();
        // a,b,c repeated three times → period 3
        let seq = ["a", "b", "c", "a", "b", "c", "a", "b", "c"];
        let mut found = None;
        for s in seq {
            found = d.observe_step(tool_sig(s)).or(found);
        }
        assert!(matches!(
            found,
            Some(LoopFinding::Oscillation { period: 3 })
        ));
    }

    #[test]
    fn repeated_failure_fires_after_three_identical_fingerprints() {
        let mut d = LoopDetector::default();
        assert!(d
            .observe_failure(Some("E0308|src/lib.rs||mismatched"))
            .is_none());
        assert!(d
            .observe_failure(Some("E0308|src/lib.rs||mismatched"))
            .is_none());
        let f = d
            .observe_failure(Some("E0308|src/lib.rs||mismatched"))
            .unwrap();
        assert!(matches!(f, LoopFinding::RepeatedFailure { count: 3, .. }));
    }

    #[test]
    fn a_passing_run_clears_the_failure_streak() {
        let mut d = LoopDetector::default();
        d.observe_failure(Some("x"));
        d.observe_failure(Some("x"));
        d.observe_failure(None); // pass
        assert!(d.observe_failure(Some("x")).is_none());
    }

    #[test]
    fn no_change_iteration_fires_when_file_state_is_static_across_acting_steps() {
        let mut d = LoopDetector::default();
        let h = ContentHash::of_bytes(b"unchanged");
        let mk = |n: &str| {
            StepSignature::default()
                .with_tool_call(n, &serde_json::json!({"n": n}))
                .with_file_state(h)
        };
        assert!(d.observe_step(mk("a")).is_none());
        assert!(d.observe_step(mk("b")).is_none());
        let f = d.observe_step(mk("c")).unwrap();
        assert!(matches!(f, LoopFinding::NoChangeIteration { .. }));
    }

    #[test]
    fn file_state_changing_resets_no_change_run() {
        let mut d = LoopDetector::default();
        let mk = |content: &str| {
            StepSignature::default()
                .with_tool_call("edit", &serde_json::json!({"c": content}))
                .with_file_state(ContentHash::of_bytes(content.as_bytes()))
        };
        for c in ["v1", "v1", "v2", "v3"] {
            assert!(
                d.observe_step(mk(c)).is_none(),
                "should not flag while file state moves"
            );
        }
    }

    #[test]
    fn frontier_stalled_fires_after_three_non_advancing_cycles() {
        let mut d = LoopDetector::default();
        let stuck = ProgressMetric {
            verification_frontier: 1,
            failure_count: 2,
            files_touched: BTreeSet::new(),
        };
        assert!(d.observe_progress(stuck.clone()).is_none()); // baseline
        assert!(d.observe_progress(stuck.clone()).is_none());
        assert!(d.observe_progress(stuck.clone()).is_none());
        let f = d.observe_progress(stuck).unwrap();
        assert!(matches!(f, LoopFinding::FrontierStalled { .. }));
    }

    #[test]
    fn progress_advance_resets_the_stall_counter() {
        let mut d = LoopDetector::default();
        let base = ProgressMetric {
            verification_frontier: 1,
            failure_count: 3,
            ..Default::default()
        };
        d.observe_progress(base.clone()); // stalled_run = 0 (baseline)
        d.observe_progress(base.clone()); // stalled_run = 1
                                          // fewer failures = progress → stalled_run back to 0
        d.observe_progress(ProgressMetric {
            failure_count: 1,
            ..base.clone()
        });
        d.observe_progress(base.clone()); // stalled_run = 1
        assert!(d.observe_progress(base).is_none()); // stalled_run = 2, still < 3
    }

    #[test]
    fn distinct_steps_never_trip_anything() {
        let mut d = LoopDetector::default();
        for i in 0..12 {
            let sig = StepSignature::default()
                .with_tool_call("edit", &serde_json::json!({"i": i}))
                .with_file_state(ContentHash::of_bytes(format!("state{i}").as_bytes()));
            assert!(d.observe_step(sig).is_none(), "distinct step {i} flagged");
        }
    }

    #[test]
    fn pure_reasoning_steps_do_not_count_as_exact_repeats() {
        let mut d = LoopDetector::default();
        for _ in 0..5 {
            assert!(d.observe_step(StepSignature::default()).is_none());
        }
    }
}
