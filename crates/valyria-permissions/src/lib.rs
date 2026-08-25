//! `valyria-permissions` — layer 3 (Execution).
//!
//! The permission engine (§22): modes, categories, argv-level command risk
//! classification (§4.9), scoped grants, and the `Authorization` capability
//! (D2) that is the *only* way a tool is allowed to execute. See
//! [`authorization`]'s module docs for why the model can never bypass this.

#![forbid(unsafe_code)]

pub mod authorization;
pub mod engine;
pub mod error;
pub mod grants;
pub mod request;
pub mod risk;
pub mod rules;

pub use authorization::{Authorization, AuthorizationKey};
pub use engine::{Decision, DecisionRecord, DecisionSource, PermissionEngine};
pub use error::{PermissionError, Result};
pub use grants::{Grant, GrantScope, GrantStore};
pub use request::{ActionKind, PermissionRequest, RiskLevel};
pub use risk::classify_command;
pub use rules::{default_decision, DefaultDecision};
