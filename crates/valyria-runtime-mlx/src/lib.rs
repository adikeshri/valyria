//! `valyria-runtime-mlx` — layer 4 (Model).
//!
//! A managed `python -m mlx_lm.server` subprocess behind a
//! [`valyria_model::ModelRuntime`]. Structurally the same crate as
//! `valyria-runtime-llamacpp`: process supervision ([`server`]) plus a
//! thin composition ([`runtime::MlxServerRuntime`]) over
//! `valyria_runtime_openai_compat::OpenAiCompatRuntime`, which owns every
//! byte of the actual wire protocol — `mlx_lm.server` exposes the same
//! `/health` + `/v1/chat/completions` shape `llama-server` does, so
//! nothing about the wire layer is MLX-specific.
//!
//! This crate resolves nothing on its own — it is handed an already
//! provisioned venv's `python` path and a local MLX model directory
//! (Hugging Face layout: `config.json`, tokenizer files, `.safetensors`
//! weights — MLX has no single-file format the way GGUF is one).
//! Provisioning that venv is `valyria-engine-store`'s job, orchestrated
//! by the caller so it can emit its own progress events;
//! `MlxServerRuntime::start` just runs it.

pub mod error;
pub mod runtime;
pub mod server;

pub use error::{MlxError, Result};
pub use runtime::MlxServerRuntime;
pub use server::{MlxServer, MlxServerConfig, DEFAULT_READY_TIMEOUT};
/// Re-exported for convenience — its canonical home is
/// `valyria_model::LocalModelServer`, shared with every other
/// local-engine adapter (`valyria-runtime-llamacpp` included).
pub use valyria_model::LocalModelServer;

/// Kept for continuity with the crate's original scaffold; the crate is
/// now implemented.
pub const PHASE: u8 = 9;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_is_recorded() {
        assert_eq!(PHASE, 9);
    }
}
