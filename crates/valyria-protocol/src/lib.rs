//! `valyria-protocol` — layer 6 (Interface).
//!
//! Versioned wire types and the `Client` boundary (§4.27, D11): the only
//! API surface `valyria-cli` (or any future desktop client) is allowed to
//! call. Phase 3 needs the typed request/response dispatch and cursor-based
//! event streaming to be real; full JSON-RPC 2.0 framing over stdio/socket,
//! `xtask schema` export, and the compat-CI-gate land in Phase 10.

#![forbid(unsafe_code)]

pub mod client;
pub mod envelope;
pub mod messages;
pub mod version;

pub use client::Client;
pub use envelope::{Request, Response};
pub use messages::{
    EventsSubscribeRequest, HelloRequest, HelloResponse, PermissionResolveRequest,
    TaskCreateRequest, TaskCreateResponse, TaskIdRequest, TaskStatusRequest, TaskStatusResponse,
    WireError, WireEvent,
};
pub use version::PROTOCOL_VERSION;
