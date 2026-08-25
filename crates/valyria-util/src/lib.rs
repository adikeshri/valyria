//! `valyria-util` — layer 0 (Foundation).
//!
//! Cross-cutting infrastructure with no domain knowledge: cancellation,
//! backoff, secret redaction, content hashing, path sanity checks, token
//! counting, tracing setup, and the injected `Clock`/`Rng` traits that make
//! the rest of the workspace deterministic under test.

#![forbid(unsafe_code)]

pub mod backoff;
pub mod cancel;
pub mod clock;
pub mod hash;
pub mod path;
pub mod redact;
pub mod rng;
pub mod token_count;
pub mod tracing_setup;

pub use backoff::Backoff;
pub use cancel::CancellationToken;
pub use clock::{Clock, FixedClock, SystemClock};
pub use hash::ContentHash;
pub use redact::{looks_like_secret, redact, shannon_entropy};
pub use rng::{DeterministicRng, OsRng, Rng};
pub use token_count::{HeuristicTokenCounter, TokenCounter};
pub use tracing_setup::{init_tracing, LogFormat};
