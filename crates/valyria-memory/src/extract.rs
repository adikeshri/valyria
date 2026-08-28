//! Turning what the runtime observed into repository memory (§4.19,
//! §29-30).
//!
//! This is intentionally a small set of high-precision heuristics, not a
//! summarizer: a command that ran and exited clean is worth remembering
//! as "this works here"; a command that has failed several times running
//! is worth remembering as a pitfall. Everything produced is
//! [`MemoryAuthor::Agent`] — [`Trust::Evidence`](valyria_types::Trust) —
//! so it can inform the next step without commanding it, and every entry
//! carries the provenance it was derived from.

use crate::entry::{MemoryEntry, MemoryKind, MemoryScope};

/// What kind of thing was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservationKind {
    /// A shell command the agent ran.
    CommandRun,
    /// A test command specifically.
    TestRun,
    /// A linter/formatter.
    LintRun,
    /// A build/compile command.
    BuildRun,
}

/// One thing the runtime saw happen, distilled to what extraction needs.
#[derive(Debug, Clone)]
pub struct Observation {
    pub kind: ObservationKind,
    /// The command line (or the salient part of it).
    pub detail: String,
    /// Whether the most recent run of it succeeded.
    pub success: bool,
    /// How many times it has been observed to fail in this task.
    pub failures: u32,
    /// Where this was seen — a task id, a tool invocation id.
    pub provenance: String,
}

impl Observation {
    pub fn new(kind: ObservationKind, detail: impl Into<String>, success: bool) -> Self {
        Self {
            kind,
            detail: detail.into(),
            success,
            failures: 0,
            provenance: String::new(),
        }
    }

    pub fn with_failures(mut self, failures: u32) -> Self {
        self.failures = failures;
        self
    }

    pub fn with_provenance(mut self, provenance: impl Into<String>) -> Self {
        self.provenance = provenance.into();
        self
    }
}

/// Failure count at or above which a repeatedly-failing command becomes a
/// pitfall memory.
const PITFALL_THRESHOLD: u32 = 3;

/// Extract repository-scoped memory from a batch of observations. Deduped
/// by resulting text; nothing is written to a store here — the caller
/// decides.
pub fn extract(observations: &[Observation], now_ms: i64) -> Vec<MemoryEntry> {
    let mut out: Vec<MemoryEntry> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for obs in observations {
        let detail = obs.detail.trim();
        if detail.is_empty() {
            continue;
        }

        let candidate = if obs.failures >= PITFALL_THRESHOLD {
            Some((
                MemoryKind::Pitfall,
                format!(
                    "`{detail}` has failed {} times — check it before relying on it",
                    obs.failures
                ),
                0.5,
            ))
        } else if obs.success {
            let noun = match obs.kind {
                ObservationKind::TestRun => "test command",
                ObservationKind::LintRun => "lint command",
                ObservationKind::BuildRun => "build command",
                ObservationKind::CommandRun => "command",
            };
            Some((
                MemoryKind::Command,
                format!("`{detail}` is a working {noun} in this repository"),
                0.6,
            ))
        } else {
            None
        };

        let Some((kind, text, confidence)) = candidate else {
            continue;
        };
        if !seen.insert(text.clone()) {
            continue;
        }
        let provenance = if obs.provenance.is_empty() {
            "extraction".to_string()
        } else {
            obs.provenance.clone()
        };
        out.push(MemoryEntry::agent(
            MemoryScope::Repository,
            kind,
            text,
            provenance,
            confidence,
            now_ms,
        ));
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_successful_test_command_becomes_a_command_memory() {
        let obs = vec![
            Observation::new(ObservationKind::TestRun, "cargo test --workspace", true)
                .with_provenance("task_1"),
        ];
        let mem = extract(&obs, 0);
        assert_eq!(mem.len(), 1);
        assert_eq!(mem[0].kind, MemoryKind::Command);
        assert!(mem[0].text.contains("cargo test --workspace"));
        assert_eq!(mem[0].provenance, "task_1");
        assert_eq!(mem[0].author, crate::MemoryAuthor::Agent);
    }

    #[test]
    fn a_repeatedly_failing_command_becomes_a_pitfall() {
        let obs = vec![
            Observation::new(ObservationKind::BuildRun, "make release", false).with_failures(4),
        ];
        let mem = extract(&obs, 0);
        assert_eq!(mem.len(), 1);
        assert_eq!(mem[0].kind, MemoryKind::Pitfall);
    }

    #[test]
    fn a_single_failure_produces_nothing() {
        let obs = vec![Observation::new(
            ObservationKind::CommandRun,
            "flaky.sh",
            false,
        )];
        assert!(extract(&obs, 0).is_empty());
    }

    #[test]
    fn identical_extractions_are_deduped() {
        let obs = vec![
            Observation::new(ObservationKind::TestRun, "pytest", true),
            Observation::new(ObservationKind::TestRun, "pytest", true),
        ];
        assert_eq!(extract(&obs, 0).len(), 1);
    }

    #[test]
    fn everything_extracted_is_evidence_trust() {
        let obs = vec![Observation::new(
            ObservationKind::TestRun,
            "go test ./...",
            true,
        )];
        let mem = extract(&obs, 0);
        assert_eq!(mem[0].trust(), valyria_types::Trust::Evidence);
    }
}
