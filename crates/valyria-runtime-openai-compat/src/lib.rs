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
//! `reqwest`-backed transport is a small isolated impl left out of the
//! offline build (Phase 9 scope note; see `docs/ROADMAP.md`).

#![forbid(unsafe_code)]

pub mod runtime;
pub mod transport;
pub mod wire;

pub use runtime::OpenAiCompatRuntime;
pub use transport::{HttpError, HttpResult, HttpTransport, MockTransport};

/// Kept for backwards compatibility with the scaffold; the crate is now
/// implemented.
pub const PHASE: u8 = 9;
