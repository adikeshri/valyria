//! The sequenced, durable, fan-out event bus (§43).
//!
//! Every event is written to SQLite (via `valyria-store`) before it is
//! broadcast live — events *are* the projection of the journal (D1), so
//! durability has to come first, not as an afterthought. Live subscribers
//! get a bounded [`tokio::sync::broadcast`] channel; a subscriber that
//! falls behind sees an explicit `Lagged` marker rather than silently
//! missing events, and can always recover by re-subscribing with
//! `since` set to the last `seq` it successfully processed — the backlog
//! read from SQLite makes that gap-free.

use std::collections::VecDeque;
use std::sync::Arc;

use tokio::sync::broadcast;
use valyria_store::{Migration, Store};

use crate::envelope::{EventEnvelope, NewEvent, Seq};
use crate::error::{EventsError, Result};

pub const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    description: "create events table",
    sql: "CREATE TABLE events (
        seq INTEGER PRIMARY KEY AUTOINCREMENT,
        id TEXT NOT NULL,
        task_id TEXT,
        ts_ms INTEGER NOT NULL,
        span TEXT,
        kind TEXT NOT NULL,
        payload TEXT NOT NULL
    );
    CREATE INDEX events_task_id ON events(task_id);",
}];

/// Bounded live fan-out capacity. Sized generously for a local, single-user
/// runtime; a subscriber that lags this far behind is almost certainly
/// disconnected, not just slow, and re-subscribing from its last seq is the
/// correct recovery path either way.
const LIVE_CHANNEL_CAPACITY: usize = 4096;

pub struct EventBus {
    store: Arc<Store>,
    live: broadcast::Sender<EventEnvelope>,
}

impl EventBus {
    pub fn new(store: Arc<Store>) -> Self {
        let (live, _) = broadcast::channel(LIVE_CHANNEL_CAPACITY);
        Self { store, live }
    }

    /// Persist `event` and broadcast it to live subscribers. Returns the
    /// envelope with its assigned, durable `seq`.
    pub async fn append(&self, event: NewEvent) -> Result<EventEnvelope> {
        let id = valyria_types::EventId::new();
        let ts = valyria_types::Timestamp::now();
        let task_id_str = event.task_id.map(|t| t.to_string());
        let kind_str = event.kind.as_str().to_string();
        let payload_str = event.payload.to_string();

        let seq: u64 = self
            .store
            .call({
                let id = id.to_string();
                let span = event.span.clone();
                move |conn| {
                    conn.execute(
                        "INSERT INTO events (id, task_id, ts_ms, span, kind, payload) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![id, task_id_str, ts.as_millis() as i64, span, kind_str, payload_str],
                    )?;
                    Ok(conn.last_insert_rowid() as u64)
                }
            })
            .await?;

        let envelope = EventEnvelope {
            seq: Seq(seq),
            id,
            task_id: event.task_id,
            ts,
            span: event.span,
            kind: event.kind,
            payload: event.payload,
        };

        // A `send` error here only means "nobody is currently subscribed",
        // which is a normal, non-error state — the durable write above is
        // what actually matters.
        let _ = self.live.send(envelope.clone());

        Ok(envelope)
    }

    /// Read every persisted event with `seq > since`, in order.
    pub async fn replay_since(&self, since: Seq) -> Result<Vec<EventEnvelope>> {
        self.store
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT seq, id, task_id, ts_ms, span, kind, payload FROM events WHERE seq > ?1 ORDER BY seq ASC",
                )?;
                let rows = stmt
                    .query_map([since.0 as i64], row_to_envelope)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await
            .map_err(EventsError::from)?
            .into_iter()
            .collect::<Result<Vec<_>>>()
    }

    /// Subscribe for live events, with a gap-free backlog of everything
    /// persisted since `since`. Subscribing to the live channel happens
    /// *before* reading the backlog, so there is no window in which an
    /// event could be missed between the two steps (overlap is possible
    /// and is de-duplicated by [`Subscription::recv`]).
    pub async fn subscribe_since(&self, since: Seq) -> Result<Subscription> {
        let live = self.live.subscribe();
        let backlog = self.replay_since(since).await?;
        Ok(Subscription {
            backlog: backlog.into(),
            live,
            last_delivered: since,
        })
    }
}

fn row_to_envelope(row: &rusqlite::Row<'_>) -> rusqlite::Result<Result<EventEnvelope>> {
    let seq: i64 = row.get(0)?;
    let id_str: String = row.get(1)?;
    let task_id_str: Option<String> = row.get(2)?;
    let ts_ms: i64 = row.get(3)?;
    let span: Option<String> = row.get(4)?;
    let kind_str: String = row.get(5)?;
    let payload_str: String = row.get(6)?;

    let parsed = (|| -> Result<EventEnvelope> {
        let id = id_str
            .parse()
            .map_err(|_| EventsError::Corrupt(format!("bad event id {id_str}")))?;
        let task_id = task_id_str
            .map(|s| s.parse())
            .transpose()
            .map_err(|_| EventsError::Corrupt("bad task id".into()))?;
        let kind: crate::kind::EventKind =
            serde_json::from_value(serde_json::Value::String(kind_str.clone()))
                .map_err(|_| EventsError::Corrupt(format!("unknown event kind {kind_str}")))?;
        let payload: serde_json::Value = serde_json::from_str(&payload_str)
            .map_err(|_| EventsError::Corrupt("bad payload json".into()))?;
        Ok(EventEnvelope {
            seq: Seq(seq as u64),
            id,
            task_id,
            ts: valyria_types::Timestamp::from_millis(ts_ms as u128),
            span,
            kind,
            payload,
        })
    })();

    Ok(parsed)
}

/// A "reason this event is being delivered late/out of the ordinary flow"
/// marker, or the event itself.
#[derive(Debug)]
pub enum Delivery {
    Event(EventEnvelope),
    /// The live channel dropped events before this subscriber could read
    /// them. `resume_from` is the last seq known delivered; the caller
    /// should call [`EventBus::subscribe_since`] again with it to recover
    /// the gap from the durable log, exactly like a fresh reconnect.
    Lagged {
        resume_from: Seq,
    },
}

pub struct Subscription {
    backlog: VecDeque<EventEnvelope>,
    live: broadcast::Receiver<EventEnvelope>,
    last_delivered: Seq,
}

impl Subscription {
    pub async fn recv(&mut self) -> Result<Delivery> {
        if let Some(event) = self.backlog.pop_front() {
            self.last_delivered = event.seq;
            return Ok(Delivery::Event(event));
        }

        loop {
            match self.live.recv().await {
                Ok(event) => {
                    if event.seq <= self.last_delivered {
                        continue; // already delivered from the backlog
                    }
                    self.last_delivered = event.seq;
                    return Ok(Delivery::Event(event));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    return Ok(Delivery::Lagged {
                        resume_from: self.last_delivered,
                    });
                }
                Err(broadcast::error::RecvError::Closed) => {
                    return Err(EventsError::ShutDown);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::NewEvent;
    use crate::kind::EventKind;

    async fn bus() -> EventBus {
        let store = Store::open_in_memory(MIGRATIONS).unwrap();
        EventBus::new(Arc::new(store))
    }

    #[tokio::test]
    async fn append_assigns_increasing_seq() {
        let bus = bus().await;
        let e1 = bus
            .append(NewEvent::new(EventKind::TaskStarted, serde_json::json!({})))
            .await
            .unwrap();
        let e2 = bus
            .append(NewEvent::new(
                EventKind::TaskCompleted,
                serde_json::json!({}),
            ))
            .await
            .unwrap();
        assert!(e2.seq > e1.seq);
    }

    #[tokio::test]
    async fn replay_since_returns_only_newer_events() {
        let bus = bus().await;
        let e1 = bus
            .append(NewEvent::new(EventKind::TaskStarted, serde_json::json!({})))
            .await
            .unwrap();
        let e2 = bus
            .append(NewEvent::new(
                EventKind::TaskCompleted,
                serde_json::json!({}),
            ))
            .await
            .unwrap();

        let all = bus.replay_since(Seq::ZERO).await.unwrap();
        assert_eq!(all.len(), 2);

        let since_e1 = bus.replay_since(e1.seq).await.unwrap();
        assert_eq!(since_e1.len(), 1);
        assert_eq!(since_e1[0].seq, e2.seq);
    }

    #[tokio::test]
    async fn subscriber_reconnecting_gets_exactly_what_it_missed() {
        let bus = bus().await;
        let e1 = bus
            .append(NewEvent::new(EventKind::TaskStarted, serde_json::json!({})))
            .await
            .unwrap();

        // Client disconnects here, having seen only e1.
        let e2 = bus
            .append(NewEvent::new(EventKind::ToolStarted, serde_json::json!({})))
            .await
            .unwrap();
        let e3 = bus
            .append(NewEvent::new(
                EventKind::ToolCompleted,
                serde_json::json!({}),
            ))
            .await
            .unwrap();

        let mut sub = bus.subscribe_since(e1.seq).await.unwrap();
        let first = sub.recv().await.unwrap();
        let second = sub.recv().await.unwrap();
        match (first, second) {
            (Delivery::Event(a), Delivery::Event(b)) => {
                assert_eq!(a.seq, e2.seq);
                assert_eq!(b.seq, e3.seq);
            }
            other => panic!("expected two events, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn live_events_are_delivered_without_duplication_across_backlog_overlap() {
        let bus = bus().await;
        let mut sub = bus.subscribe_since(Seq::ZERO).await.unwrap();

        let appended = bus
            .append(NewEvent::new(EventKind::TaskStarted, serde_json::json!({})))
            .await
            .unwrap();

        let delivery = sub.recv().await.unwrap();
        match delivery {
            Delivery::Event(e) => assert_eq!(e.seq, appended.seq),
            other => panic!("expected event, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn lagged_subscriber_can_recover_via_resubscribe() {
        let bus = bus().await;
        let mut sub = bus.subscribe_since(Seq::ZERO).await.unwrap();

        // Flood past the live channel capacity without reading, forcing a lag.
        for _ in 0..(LIVE_CHANNEL_CAPACITY + 10) {
            bus.append(NewEvent::new(EventKind::ToolStarted, serde_json::json!({})))
                .await
                .unwrap();
        }

        let resume_from = loop {
            match sub.recv().await.unwrap() {
                Delivery::Lagged { resume_from } => break resume_from,
                Delivery::Event(_) => continue,
            }
        };

        // Recovery: a fresh subscription from resume_from must see every
        // event that actually happened, gap-free, from the durable log.
        let recovered = bus.replay_since(resume_from).await.unwrap();
        assert_eq!(recovered.len(), LIVE_CHANNEL_CAPACITY + 10);
    }

    #[tokio::test]
    async fn payload_and_task_id_round_trip_through_persistence() {
        let bus = bus().await;
        let task_id = valyria_types::TaskId::new();
        let payload = serde_json::json!({"objective": "fix the flaky test", "n": 42});
        bus.append(
            NewEvent::new(EventKind::TaskStarted, payload.clone())
                .for_task(task_id)
                .with_span("agent.step.1"),
        )
        .await
        .unwrap();

        let events = bus.replay_since(Seq::ZERO).await.unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].task_id, Some(task_id));
        assert_eq!(events[0].payload, payload);
        assert_eq!(events[0].span.as_deref(), Some("agent.step.1"));
    }

    #[tokio::test]
    async fn events_survive_bus_recreation_against_the_same_store() {
        let store = Arc::new(Store::open_in_memory(MIGRATIONS).unwrap());
        {
            let bus = EventBus::new(store.clone());
            bus.append(NewEvent::new(EventKind::TaskStarted, serde_json::json!({})))
                .await
                .unwrap();
        }
        let bus2 = EventBus::new(store);
        let events = bus2.replay_since(Seq::ZERO).await.unwrap();
        assert_eq!(events.len(), 1);
    }
}
