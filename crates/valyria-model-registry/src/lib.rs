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
//! `include_str!`) so the runtime works fully offline; [`signing`]'s
//! ed25519 mechanism lets a caller accept a signed remote refresh instead
//! (§ M6, [`Catalog::verify_and_parse_signed`]). Nothing here downloads,
//! loads, or runs a model — that is `valyria-model-store` and the runtime
//! adapters.

#![forbid(unsafe_code)]

pub mod card;
pub mod catalog;
pub mod error;
pub mod license;
pub mod role;
pub mod select;
pub mod signing;

pub use card::{EngineKind, ModelCard, Quantization, TransportPreference};
pub use catalog::Catalog;
/// Re-exported so a caller of [`Catalog::verify_and_parse_signed`] /
/// [`signing::sign`] doesn't need its own direct `ed25519-dalek`
/// dependency just to hold the key types those functions pass around.
pub use ed25519_dalek::{SigningKey, VerifyingKey};
pub use error::{RegistryError, Result};
pub use license::{has_license_text, license_text};
pub use role::ModelRole;
pub use select::{score_card_for_role, select_for_role, CardScore, RoleAssignment, RoleBinding};
pub use signing::{
    generate_keypair, parse_public_key_hex, sign, verify as verify_signature,
    CATALOG_PUBLIC_KEY_HEX,
};

/// Kept for backwards compatibility with the scaffold; the crate is now
/// implemented.
pub const PHASE: u8 = 9;
