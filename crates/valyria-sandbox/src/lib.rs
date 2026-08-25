//! `valyria-sandbox` — layer 1 (Platform).
//!
//! Sandboxed execution (§21, D10). Confinement is real where implemented
//! and verified (macOS, via Seatbelt — see [`macos`]'s module docs for the
//! hard-won details) and honestly reported as absent where it isn't
//! (Linux, Windows: `PermissiveSandbox`, tracked as future work rather
//! than faked). The runtime must always know and be able to state exactly
//! what confinement a run actually got.

#![forbid(unsafe_code)]

pub mod confinement;
pub mod error;
pub mod fs_guard;
pub mod launcher;
pub mod permissive;
pub mod profile;

#[cfg(target_os = "macos")]
pub mod macos;

pub use confinement::Confinement;
pub use error::{Result, SandboxError};
pub use fs_guard::{AllowlistFsGuard, FsGuard};
pub use launcher::{detect_platform_launcher, ProcessLauncher};
pub use permissive::PermissiveSandbox;
pub use profile::SandboxProfile;
