//! `valyria-runtime-llamacpp` — layer 4 (Model).
//!
//! llama.cpp adapter (in-process FFI and/or server), GBNF constrained decoding.
//!
//! Status: **deferred within Phase 9** (open decision 5 — a solo build
//! defers the MLX/CUDA adapters and FFI work). llama.cpp's default mode is
//! a managed `llama-server` subprocess, which reuses
//! `valyria_runtime_openai_compat::OpenAiCompatRuntime` wholesale once a
//! concrete `HttpTransport` exists — so this crate becomes process
//! supervision plus GBNF grammar compilation, not a second wire protocol.
//! The crate compiles and is wired into the layering check until then.

#![forbid(unsafe_code)]

/// Marks this crate as present in the workspace topology for the given phase.
/// Exists so the crate is non-empty and the layering/CI checks have something
/// real to verify before the phase implementation lands.
pub const PHASE: u8 = 9;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_is_recorded() {
        assert_eq!(PHASE, 9);
    }
}
