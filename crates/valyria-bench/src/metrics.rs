//! Per-run metrics, projected from the task journal's event stream (the
//! journal *is* the source of truth — D1). These are the numbers a
//! regression is diffed against.

use serde::{Deserialize, Serialize};
use valyria_events::{EventEnvelope, EventKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchMetrics {
    /// Wall-clock time for the whole run, measured by the harness.
    pub wall_ms: u64,
    pub state_changes: u32,
    pub model_calls: u32,
    pub tool_calls: u32,
    pub files_changed: u32,
    pub verification_runs: u32,
    pub tests_passed: u32,
    pub tests_failed: u32,
    pub progress_stalls: u32,
    /// Did the task reach a terminal state on its own (vs. the harness
    /// timing it out)?
    pub reached_terminal: bool,
}

impl BenchMetrics {
    pub fn from_events(events: &[EventEnvelope], wall_ms: u64, reached_terminal: bool) -> Self {
        let mut m = BenchMetrics {
            wall_ms,
            state_changes: 0,
            model_calls: 0,
            tool_calls: 0,
            files_changed: 0,
            verification_runs: 0,
            tests_passed: 0,
            tests_failed: 0,
            progress_stalls: 0,
            reached_terminal,
        };
        for ev in events {
            match ev.kind {
                EventKind::StateChanged => m.state_changes += 1,
                EventKind::ModelCompleted => m.model_calls += 1,
                EventKind::ToolCompleted => m.tool_calls += 1,
                EventKind::FileChanged => m.files_changed += 1,
                EventKind::VerificationEvidence => m.verification_runs += 1,
                EventKind::TestPassed => m.tests_passed += 1,
                EventKind::TestFailed => m.tests_failed += 1,
                EventKind::ProgressStalled => m.progress_stalls += 1,
                _ => {}
            }
        }
        m
    }

    /// Fields that count as "cost" for regression detection: a large
    /// jump in any of these on a task that still passes is a soft
    /// regression worth surfacing.
    pub fn cost_fields(&self) -> [(&'static str, u64); 4] {
        [
            ("model_calls", self.model_calls as u64),
            ("tool_calls", self.tool_calls as u64),
            ("verification_runs", self.verification_runs as u64),
            ("progress_stalls", self.progress_stalls as u64),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use valyria_events::{EventEnvelope, EventKind};
    use valyria_types::{EventId, TaskId, Timestamp};

    fn ev(kind: EventKind, task_id: Option<TaskId>) -> EventEnvelope {
        EventEnvelope {
            seq: valyria_events::Seq(1),
            id: EventId::new(),
            task_id,
            ts: Timestamp::now(),
            span: None,
            kind,
            payload: serde_json::Value::Null,
        }
    }

    #[test]
    fn empty_stream_is_all_zero_but_keeps_the_terminal_flag() {
        let m = BenchMetrics::from_events(&[], 42, true);
        assert_eq!(m.wall_ms, 42);
        assert!(m.reached_terminal);
        assert_eq!(m.model_calls, 0);
        assert_eq!(m.tool_calls, 0);
    }

    #[test]
    fn counts_are_projected_by_kind() {
        let t = Some(TaskId::new());
        let events = vec![
            ev(EventKind::StateChanged, t),
            ev(EventKind::StateChanged, t),
            ev(EventKind::ModelCompleted, t),
            ev(EventKind::ToolCompleted, t),
            ev(EventKind::VerificationEvidence, t),
            ev(EventKind::TestFailed, t),
            ev(EventKind::TestPassed, t),
            ev(EventKind::ProgressStalled, t),
            ev(EventKind::MemoryWritten, t),
        ];
        let m = BenchMetrics::from_events(&events, 0, false);
        assert_eq!(m.state_changes, 2);
        assert_eq!(m.model_calls, 1);
        assert_eq!(m.tool_calls, 1);
        assert_eq!(m.verification_runs, 1);
        assert_eq!(m.tests_failed, 1);
        assert_eq!(m.tests_passed, 1);
        assert_eq!(m.progress_stalls, 1);
        assert!(!m.reached_terminal);
        assert_eq!(m.cost_fields()[0], ("model_calls", 1));
    }
}
