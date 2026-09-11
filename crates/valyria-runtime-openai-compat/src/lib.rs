//! `valyria-runtime-openai-compat` — layer 4 (Model).
//!
//! A [`ModelRuntime`](valyria_model::ModelRuntime) for any local
//! OpenAI-compatible server — llama-server, vLLM, Ollama, LM Studio. This
//! is the adapter Phase 9 leans on: it needs no FFI and no Python bridge,
//! just a running server.
//!
//! HTTP is abstracted behind [`HttpTransport`] so request construction,
//! response parsing (`/v1/chat/completions`, both buffered and SSE),
//! native tool-call extraction, and mid-request / mid-stream cancellation
//! are all covered offline against [`MockTransport`]. The concrete
//! `reqwest`-backed [`ReqwestTransport`] is compiled by the default `http`
//! feature; turn it off (`--no-default-features`) for a build with no TLS
//! stack at all.

#![forbid(unsafe_code)]

pub mod runtime;
pub mod transport;
#[cfg(feature = "http")]
pub mod transport_reqwest;
pub mod wire;

pub use runtime::OpenAiCompatRuntime;
pub use transport::{HttpError, HttpResult, HttpTransport, MockTransport};
#[cfg(feature = "http")]
pub use transport_reqwest::ReqwestTransport;

/// Kept for backwards compatibility with the scaffold; the crate is now
/// implemented.
pub const PHASE: u8 = 9;
