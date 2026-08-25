//! `valyria-testkit` — layer 0 (Foundation), dev-dependency only.
//!
//! Shared test infrastructure: disposable temp workspaces for fixture
//! repos, golden-file assertions, and re-exports of the deterministic
//! `Clock`/`Rng` from `valyria-util` so test code has one crate to import
//! for "make this test reproducible". This crate is never a normal
//! dependency of anything (see the workspace layering check) — only ever a
//! `[dev-dependencies]` entry, which is what lets crates it depends on for
//! fixtures (like `valyria-util`) also use it in their own tests without
//! forming a cycle.

#![forbid(unsafe_code)]

pub mod golden;
pub mod workspace;

pub use golden::{assert_golden, assert_golden_with_mode};
pub use valyria_util::{DeterministicRng, FixedClock};
pub use workspace::TempWorkspace;
