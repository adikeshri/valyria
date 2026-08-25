//! `valyria-app` — layer 6 (Interface).
//!
//! Application layer: workspace registry, session mgmt, wiring, daemon.
//!
//! Status: scaffolded per the build plan (docs/PLAN.md, Phase 3). The crate
//! compiles and is wired into the workspace layering check; full implementation
//! lands in its designated phase.

#![forbid(unsafe_code)]

/// Marks this crate as present in the workspace topology for the given phase.
/// Exists so the crate is non-empty and the layering/CI checks have something
/// real to verify before the phase implementation lands.
pub const PHASE: u8 = 3;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_is_recorded() {
        assert_eq!(PHASE, 3);
    }
}
