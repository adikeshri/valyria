//! `valyria-instructions` — layer 5 (Agent).
//!
//! Instruction discovery, authority order, trust assignment (§4.18, §33).
//!
//! The runtime treats the repository as a place that *tells it how to
//! work* — but not every such file speaks with the same authority, and one
//! of them (`README`, `CONTRIBUTING.md`) is only advisory. This crate
//! finds the instruction files under a workspace, orders them by a fixed,
//! documented authority order, assigns each a [`Trust`](valyria_types::Trust)
//! level, and reports where two of them contradict each other so the
//! client can surface it.
//!
//! What it deliberately does not do: decide the *runtime policy* (that is
//! compiled into `valyria-context` and always outranks anything found on
//! disk), or merge instructions into a prompt (that is prompt assembly's
//! job, and the trust levels this crate assigns are exactly what makes it
//! safe). Everything here is pure over a filesystem read — no network, no
//! model, no state.

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod authority;
pub mod conflict;
pub mod discover;
pub mod error;
pub mod source;

pub use authority::Authority;
pub use conflict::InstructionConflict;
pub use discover::{Discovery, DEFAULT_MAX_BYTES};
pub use error::{InstructionError, Result};
pub use source::{FileFingerprint, InstructionFingerprint, InstructionSet, InstructionSource};

/// The build phase this crate's implementation belongs to
/// ([docs/PLAN.md §5](../docs/PLAN.md)).
pub const PHASE: u8 = 6;

#[cfg(test)]
mod tests {
    #[test]
    fn phase_is_recorded() {
        assert_eq!(super::PHASE, 6);
    }
}
