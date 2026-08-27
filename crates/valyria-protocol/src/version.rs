//! Semantic versioning of the protocol itself (§4.27), independent of
//! `valyria`'s own crate version. `Hello` negotiates this; a real
//! machine-checked compatibility suite and `xtask schema` export land in
//! Phase 10 — bump this deliberately once they exist.
pub const PROTOCOL_VERSION: &str = "0.1.0";
