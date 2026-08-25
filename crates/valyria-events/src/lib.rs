//! `valyria-events` — layer 0 (Foundation).
//!
//! The durable, sequenced, fan-out event bus (§43). Events are the
//! projection of durable state (D1): every event is persisted before it is
//! broadcast, and a client that reconnects with a `since` cursor gets
//! exactly what it missed via [`bus::EventBus::subscribe_since`] — never a
//! gap, never a silent drop.

#![forbid(unsafe_code)]

pub mod bus;
pub mod envelope;
pub mod error;
pub mod kind;

pub use bus::{Delivery, EventBus, Subscription, MIGRATIONS};
pub use envelope::{EventEnvelope, NewEvent, Seq};
pub use error::{EventsError, Result};
pub use kind::EventKind;
