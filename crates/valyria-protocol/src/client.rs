//! The `Client` trait: the *only* API surface a `valyria-cli` command may
//! call (D11 — the CLI must not contain agent orchestration logic, enforced
//! by the fact that it can only speak this trait). Phase 3 ships exactly
//! one implementation, `valyria_app::EmbeddedClient`, which runs the
//! runtime in-process; a daemon transport (Phase 10) implements the same
//! trait against a socket, and no call site anywhere needs to change.

use futures::stream::BoxStream;

use crate::envelope::{Request, Response};
use crate::messages::WireEvent;

#[async_trait::async_trait]
pub trait Client: Send + Sync {
    async fn call(&self, req: Request) -> Response;

    /// Cursor-based resume (§4.27): a client reconnecting with its last
    /// known `since` gets exactly what it missed. The embedded
    /// implementation backs this with `EventBus::subscribe_since` and
    /// transparently resubscribes on `Delivery::Lagged`, so no caller here
    /// ever observes a gap.
    async fn subscribe_events(&self, since: u64) -> BoxStream<'static, WireEvent>;
}
