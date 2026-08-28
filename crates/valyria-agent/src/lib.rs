//! `valyria-agent` — layer 5 (Agent).
//!
//! The step machine's driver (§4.24, D1): `AgentDriver` executes one
//! `AgentState` at a time against `valyria-task`'s journal and
//! `valyria-tools`' permission-gated runtime, so every effect is durable
//! before it runs and a crash between any two journal writes is
//! recoverable by the same driver on restart.

#![forbid(unsafe_code)]

pub mod action;
pub mod driver;
pub mod error;
pub mod loop_detect;
pub mod repair;

pub use action::ActionRequest;
pub use driver::AgentDriver;
pub use error::{AgentError, Result};
pub use loop_detect::{DetectorConfig, LoopDetector, LoopFinding, ProgressMetric, StepSignature};
pub use repair::{RepairAttempt, RepairDecision, RepairLedger, RepairOutcome};
