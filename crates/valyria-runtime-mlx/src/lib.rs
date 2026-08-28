//! `valyria-runtime-mlx` — layer 4 (Model).
//!
//! Apple-silicon MLX adapter.
//!
//! Status: **deferred within Phase 9** (open decision 5 — a solo build
//! defers the MLX/CUDA adapters). MLX is Python-side, so this adapter is a
//! managed `mlx-lm` server subprocess with a strict handshake; it reuses
//! the OpenAI-compatible client once a concrete `HttpTransport` exists. The
//! crate compiles and is wired into the layering check until then.

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
