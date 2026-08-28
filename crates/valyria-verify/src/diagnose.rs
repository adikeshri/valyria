//! Diagnosis (§29): from a set of parsed [`Failure`]s to a ranked list of
//! *suspect files* — the ones a repair should look at first.
//!
//! The signal is an intersection: a file that a failure points at **and**
//! that this task changed is a much stronger suspect than either fact
//! alone. A changed file in the graph neighbourhood of a failure location
//! is next. A failing test's own file is a weak suspect (the bug is
//! usually in what it tests, not the test). Everything else is noise and
//! is left out — "only the distilled subset enters context."

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::parse::{Failure, FailureKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuspectReason {
    /// A failure's primary/secondary location is in this file.
    FailureLocation,
    /// This task's change ledger touched this file.
    RecentlyChanged,
    /// A failure location is in the graph neighbourhood of this changed
    /// file (it calls / is called by the failing code).
    GraphNeighbor,
    /// This file contains the failing test itself (weak).
    FailingTestFile,
}

impl SuspectReason {
    fn weight(self) -> f32 {
        match self {
            SuspectReason::FailureLocation => 1.0,
            SuspectReason::RecentlyChanged => 0.8,
            SuspectReason::GraphNeighbor => 0.5,
            SuspectReason::FailingTestFile => 0.2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Suspect {
    pub path: PathBuf,
    pub reasons: Vec<SuspectReason>,
    pub score: f32,
}

/// The distilled result of a failing verification run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnosis {
    pub failures: Vec<Failure>,
    pub suspects: Vec<Suspect>,
    pub summary: String,
}

impl Diagnosis {
    /// The single narrowest failing check to re-run first during repair
    /// (§30): a specific failing test if we have one, else nothing (the
    /// caller re-runs the whole failing command).
    pub fn narrowest_failing_test(&self) -> Option<&str> {
        self.failures.iter().find_map(|f| f.failing_test.as_deref())
    }

    pub fn top_suspects(&self, n: usize) -> impl Iterator<Item = &Suspect> {
        self.suspects.iter().take(n)
    }

    /// A compact digest for the repair prompt's context — failures plus
    /// the top few suspects, never raw output.
    pub fn context_digest(&self, max_failures: usize, max_suspects: usize) -> String {
        let mut s = self.summary.clone();
        for f in self.failures.iter().take(max_failures) {
            s.push_str("\n• ");
            s.push_str(&f.message);
            if let Some(loc) = &f.primary_location {
                s.push_str(&format!(
                    " [{}{}]",
                    loc.file.display(),
                    loc.line.map(|l| format!(":{l}")).unwrap_or_default()
                ));
            }
            if let Some(a) = &f.assertion {
                if let (Some(e), Some(act)) = (&a.expected, &a.actual) {
                    s.push_str(&format!(" (expected {e}, got {act})"));
                }
            }
        }
        if !self.suspects.is_empty() {
            s.push_str("\nsuspect files: ");
            s.push_str(
                &self
                    .suspects
                    .iter()
                    .take(max_suspects)
                    .map(|s| s.path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        s
    }

    /// A stable identity for "this same diagnosis again" (loop detection,
    /// repair ledger): the sorted set of failure fingerprints.
    pub fn fingerprint(&self) -> String {
        let mut fps: Vec<String> = self.failures.iter().map(|f| f.fingerprint()).collect();
        fps.sort();
        fps.dedup();
        fps.join(";;")
    }
}

/// `graph_neighbors` is `(changed_file, file_in_its_neighbourhood)` pairs
/// the caller derived from the knowledge graph. Both `changed_files` and
/// the pairs use workspace-relative paths.
pub fn diagnose(
    failures: &[Failure],
    changed_files: &[PathBuf],
    graph_neighbors: &[(PathBuf, PathBuf)],
) -> Diagnosis {
    let mut acc: BTreeMap<PathBuf, Vec<SuspectReason>> = BTreeMap::new();

    let mut add = |path: PathBuf, reason: SuspectReason| {
        let entry = acc.entry(normalize(&path)).or_default();
        if !entry.contains(&reason) {
            entry.push(reason);
        }
    };

    let changed: Vec<PathBuf> = changed_files.iter().map(|p| normalize(p)).collect();

    for failure in failures {
        // Failing test file — weak.
        if failure.kind == FailureKind::TestFailure || failure.kind == FailureKind::TestPanic {
            if let Some(loc) = &failure.primary_location {
                if is_test_path(&loc.file) {
                    add(loc.file.clone(), SuspectReason::FailingTestFile);
                }
            }
        }

        let mut locs: Vec<&Path> = Vec::new();
        if let Some(loc) = &failure.primary_location {
            locs.push(&loc.file);
        }
        locs.extend(failure.secondary_locations.iter().map(|l| l.file.as_path()));

        for loc in locs {
            let loc = normalize(loc);
            if !is_test_path(&loc) {
                add(loc.clone(), SuspectReason::FailureLocation);
            }
            // Graph: a changed file whose neighbourhood contains this
            // location.
            for (changed_file, neighbor) in graph_neighbors {
                if normalize(neighbor) == loc {
                    add(normalize(changed_file), SuspectReason::GraphNeighbor);
                }
            }
        }
    }

    for path in &changed {
        add(path.clone(), SuspectReason::RecentlyChanged);
    }

    let mut suspects: Vec<Suspect> = acc
        .into_iter()
        .map(|(path, reasons)| {
            // Intersection bonus: a file that is both a failure location
            // and recently changed is what we most want surfaced.
            let mut score: f32 = reasons.iter().map(|r| r.weight()).sum();
            let is_loc = reasons.contains(&SuspectReason::FailureLocation);
            let is_changed = reasons.contains(&SuspectReason::RecentlyChanged);
            if is_loc && is_changed {
                score += 1.0;
            }
            Suspect {
                path,
                reasons,
                score,
            }
        })
        // A file that is *only* a failing test file with nothing else is
        // barely a suspect; keep it but it will rank last.
        .collect();

    suspects.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.path.cmp(&b.path))
    });

    let summary = build_summary(failures, &suspects);
    Diagnosis {
        failures: failures.to_vec(),
        suspects,
        summary,
    }
}

fn build_summary(failures: &[Failure], suspects: &[Suspect]) -> String {
    if failures.is_empty() {
        return "no failures parsed from the run".into();
    }
    let kinds: Vec<String> = {
        let mut seen = Vec::new();
        for f in failures {
            let k = format!("{:?}", f.kind);
            if !seen.contains(&k) {
                seen.push(k);
            }
        }
        seen
    };
    let lead = suspects
        .first()
        .map(|s| format!("; most likely in {}", s.path.display()))
        .unwrap_or_default();
    format!(
        "{} failure(s) [{}]{}",
        failures.len(),
        kinds.join(", "),
        lead
    )
}

fn normalize(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    PathBuf::from(s.trim_start_matches("./"))
}

fn is_test_path(p: &Path) -> bool {
    let s = p.to_string_lossy();
    s.contains("/tests/")
        || s.starts_with("tests/")
        || s.contains("test_")
        || s.ends_with("_test.go")
        || s.ends_with(".test.js")
        || s.ends_with(".test.ts")
        || s.ends_with("_test.py")
        || s.contains("spec")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Location;

    fn test_failure(file: &str, line: u32, test: &str) -> Failure {
        let mut f = Failure::new(FailureKind::TestFailure, "boom");
        f.primary_location = Some(Location::at(file, line, 1));
        f.failing_test = Some(test.into());
        f
    }

    fn compile_failure(file: &str, line: u32) -> Failure {
        let mut f = Failure::new(FailureKind::CompileError, "mismatched types");
        f.primary_location = Some(Location::at(file, line, 1));
        f
    }

    #[test]
    fn changed_file_at_a_failure_location_ranks_first() {
        let failures = vec![compile_failure("src/math.rs", 10)];
        let changed = vec![PathBuf::from("src/math.rs"), PathBuf::from("src/other.rs")];
        let d = diagnose(&failures, &changed, &[]);
        assert_eq!(d.suspects[0].path, PathBuf::from("src/math.rs"));
        assert!(d.suspects[0]
            .reasons
            .contains(&SuspectReason::FailureLocation));
        assert!(d.suspects[0]
            .reasons
            .contains(&SuspectReason::RecentlyChanged));
        // intersection bonus makes it clearly beat the merely-changed file
        assert!(d.suspects[0].score > d.suspects[1].score + 0.9);
    }

    #[test]
    fn graph_neighbor_of_a_failure_becomes_a_suspect() {
        let failures = vec![compile_failure("src/lib.rs", 3)];
        let changed = vec![PathBuf::from("src/helper.rs")];
        let neighbors = vec![(PathBuf::from("src/helper.rs"), PathBuf::from("src/lib.rs"))];
        let d = diagnose(&failures, &changed, &neighbors);
        let helper = d
            .suspects
            .iter()
            .find(|s| s.path == Path::new("src/helper.rs"))
            .unwrap();
        assert!(helper.reasons.contains(&SuspectReason::GraphNeighbor));
        assert!(helper.reasons.contains(&SuspectReason::RecentlyChanged));
    }

    #[test]
    fn failing_test_file_is_a_weak_suspect_not_the_top() {
        let failures = vec![test_failure("tests/math_test.rs", 5, "adds")];
        let changed = vec![PathBuf::from("src/math.rs")];
        let d = diagnose(&failures, &changed, &[]);
        assert_eq!(d.suspects[0].path, PathBuf::from("src/math.rs"));
        let test_file = d
            .suspects
            .iter()
            .find(|s| s.path == Path::new("tests/math_test.rs"));
        assert!(test_file.is_some());
        assert!(test_file.unwrap().score < d.suspects[0].score);
    }

    #[test]
    fn normalizes_leading_dot_slash() {
        let failures = vec![compile_failure("./src/a.rs", 1)];
        let d = diagnose(&failures, &[PathBuf::from("src/a.rs")], &[]);
        assert_eq!(d.suspects.len(), 1);
        assert_eq!(d.suspects[0].path, PathBuf::from("src/a.rs"));
    }

    #[test]
    fn digest_and_fingerprint_are_stable_and_bounded() {
        let failures = vec![
            compile_failure("src/a.rs", 1),
            compile_failure("src/b.rs", 2),
            compile_failure("src/c.rs", 3),
        ];
        let d = diagnose(&failures, &[PathBuf::from("src/a.rs")], &[]);
        let digest = d.context_digest(2, 2);
        assert!(digest.contains("src/a.rs"));
        assert!(!digest.contains("src/c.rs")); // capped at 2 failures
        let d2 = diagnose(&failures, &[PathBuf::from("src/a.rs")], &[]);
        assert_eq!(d.fingerprint(), d2.fingerprint());
    }

    #[test]
    fn empty_failures_produce_an_honest_summary() {
        let d = diagnose(&[], &[], &[]);
        assert!(d.suspects.is_empty());
        assert!(d.summary.contains("no failures"));
        assert!(d.narrowest_failing_test().is_none());
    }
}
