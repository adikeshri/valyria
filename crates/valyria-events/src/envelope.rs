//! The event envelope: what actually flows through the bus and what gets
//! persisted for replay.

use serde::{Deserialize, Serialize};
use valyria_types::{EventId, TaskId, Timestamp};

use crate::kind::EventKind;

/// A durable, gap-free sequence number. Assigned by the store (SQLite
/// `AUTOINCREMENT`), never by the caller — that's what makes cursor-based
/// resume (`since: Seq`) correct: a client that reconnects with the last
/// seq it saw gets exactly what it missed, never a gap or a duplicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Seq(pub u64);

impl Seq {
    pub const ZERO: Seq = Seq(0);
}

impl std::fmt::Display for Seq {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "seq:{}", self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub seq: Seq,
    pub id: EventId,
    /// Not every event is task-scoped (e.g. `ResourcePressure` is
    /// workspace/runtime-wide), so this is optional.
    pub task_id: Option<TaskId>,
    pub ts: Timestamp,
    /// The tracing span id active when this event was emitted, so a log
    /// line can be tied back to the exact protocol event (observability
    /// convention, §3).
    pub span: Option<String>,
    pub kind: EventKind,
    pub payload: serde_json::Value,
}

/// A new event before it has been assigned a durable `seq` — what callers
/// construct and hand to [`crate::bus::EventBus::append`].
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub task_id: Option<TaskId>,
    pub span: Option<String>,
    pub kind: EventKind,
    pub payload: serde_json::Value,
}

impl NewEvent {
    pub fn new(kind: EventKind, payload: serde_json::Value) -> Self {
        Self {
            task_id: None,
            span: None,
            kind,
            payload,
        }
    }

    pub fn for_task(mut self, task_id: TaskId) -> Self {
        self.task_id = Some(task_id);
        self
    }

    pub fn with_span(mut self, span: impl Into<String>) -> Self {
        self.span = Some(span.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_orders_numerically() {
        assert!(Seq(1) < Seq(2));
        assert!(Seq::ZERO < Seq(1));
    }

    #[test]
    fn new_event_builder_sets_optional_fields() {
        let task_id = TaskId::new();
        let ev = NewEvent::new(
            EventKind::TaskStarted,
            serde_json::json!({"objective": "fix bug"}),
        )
        .for_task(task_id)
        .with_span("agent.step.7");
        assert_eq!(ev.task_id, Some(task_id));
        assert_eq!(ev.span.as_deref(), Some("agent.step.7"));
    }
}
