//! `valyria-config` — layer 0 (Foundation).
//!
//! Layered configuration resolution (§4.3): compiled defaults -> global ->
//! workspace -> env -> per-task overrides, later winning, every effective
//! leaf value's origin recorded so `valyria config` can answer "where did
//! this come from?". Also owns the policy floor: config can tighten access
//! below the floor, never loosen past it.

#![forbid(unsafe_code)]

pub mod env_layer;
pub mod error;
pub mod floor;
pub mod merge;
pub mod origin;
pub mod resolver;
pub mod settings;

pub use error::{ConfigError, Result};
pub use floor::PolicyFloor;
pub use origin::{ConfigOrigin, OriginMap};
pub use resolver::{ConfigResolver, Resolved};
pub use settings::{LogFormat, LogSettings, PermissionSettings, Settings};
