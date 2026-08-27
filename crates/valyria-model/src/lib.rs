//! `valyria-model` — layer 4 (Model).
//!
//! The `ModelRuntime` trait (§4.20) and the adapter-agnostic message,
//! sampling, request, and completion vocabulary every adapter (fake,
//! llama.cpp, MLX, OpenAI-compatible) speaks. This crate defines the
//! contract only — no adapter lives here (see `valyria-runtime-fake` and,
//! from Phase 9, the real adapters).

#![forbid(unsafe_code)]

pub mod capabilities;
pub mod completion;
pub mod error;
pub mod message;
pub mod request;
pub mod runtime;
pub mod sampling;

pub use capabilities::{Capabilities, Health};
pub use completion::{Chunk, Completion, FinishReason, TokenUsage};
pub use error::{ModelError, Result};
pub use message::{Message, Role, ToolCall, ToolSpec};
pub use request::GenerateRequest;
pub use runtime::ModelRuntime;
pub use sampling::SamplingParams;
