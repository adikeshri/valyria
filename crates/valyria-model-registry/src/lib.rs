//! `valyria-model-registry` — layer 4 (Model).
//!
//! The model catalog (§4.21, §37): a static description of every model the
//! runtime knows how to run, plus the two questions the rest of the system
//! asks of it —
//!
//! 1. *"Can this machine run this model?"* — [`select::score_card_for_role`]
//!    and [`select::select_for_role`], built on `valyria_hardware::fits` so
//!    fit is judged against **measured available** memory, never total.
//! 2. *"Which model should serve this role?"* — [`RoleBinding`] with an
//!    ordered fallback chain, so a missing or unfit primary escalates to a
//!    named alternative rather than failing the task.
//!
//! The catalog ships **embedded** (`catalog.json`, compiled in via
//! `include_str!`) so the runtime works fully offline; a signed remote
//! refresh is a Phase 10 concern. Nothing here downloads, loads, or runs a
//! model — that is `valyria-model-store` and the runtime adapters.

#![forbid(unsafe_code)]

pub mod card;
pub mod catalog;
pub mod error;
pub mod license;
pub mod role;
pub mod select;

pub use card::{ModelCard, Quantization, TransportPreference};
pub use catalog::Catalog;
pub use error::{RegistryError, Result};
pub use license::{has_license_text, license_text};
pub use role::ModelRole;
pub use select::{score_card_for_role, select_for_role, CardScore, RoleAssignment, RoleBinding};

/// Kept for backwards compatibility with the scaffold; the crate is now
/// implemented.
pub const PHASE: u8 = 9;
