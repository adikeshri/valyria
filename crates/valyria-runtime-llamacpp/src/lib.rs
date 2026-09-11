//! `valyria-runtime-llamacpp` — layer 4 (Model).
//!
//! A managed `llama-server` subprocess behind a [`valyria_model::
//! ModelRuntime`]. As the crate's original scaffold doc always said it
//! would: llama.cpp's default mode is a managed server, so this crate is
//! process supervision ([`server`]) plus a thin composition
//! ([`runtime::LlamaServerRuntime`]) over
//! `valyria_runtime_openai_compat::OpenAiCompatRuntime`, which owns every
//! byte of the actual wire protocol.
//!
//! This crate resolves nothing on its own — it is handed an already
//! fetched `llama-server` binary path and a model's weights path. Finding
//! (and, the first time, downloading) that binary is `valyria-engine-
//! store`'s job, orchestrated by the caller so it can emit its own
//! progress events; `LlamaServerRuntime::start` just runs it.

pub mod error;
pub mod runtime;
pub mod server;

pub use error::{LlamaError, Result};
pub use runtime::{LlamaServerRuntime, LocalModelServer};
pub use server::{LlamaServer, LlamaServerConfig, DEFAULT_READY_TIMEOUT};

/// Kept for backwards compatibility with the scaffold; the crate is now
/// implemented.
pub const PHASE: u8 = 9;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_is_recorded() {
        assert_eq!(PHASE, 9);
    }
}
