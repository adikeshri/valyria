//! `valyria-tools` — layer 3 (Execution).
//!
//! The tool runtime (§17, §18): the `Tool` trait, structured descriptors,
//! the permission-gated invocation path (D2 enforcement point), and the
//! 18 first-class tools. Fifteen are fully implemented; `search`,
//! `symbol_search`, and `git_blame` are registered with real descriptors
//! but return a clear not-yet-implemented error pending the repository
//! index (Phase 4/5) and a `valyria-git` blame pass.

#![forbid(unsafe_code)]

pub mod canonical;
pub mod ctx;
pub mod descriptor;
pub mod error;
pub mod invocation;
pub mod outcome;
pub mod runtime;
pub mod tool_trait;
pub mod tools;

pub use canonical::canonical_input_hash;
pub use ctx::ToolCtx;
pub use descriptor::{SideEffect, ToolDescriptor};
pub use error::{Result, ToolError};
pub use invocation::ToolInvocationRecord;
pub use outcome::ToolOutcome;
pub use runtime::{InvocationResult, ToolRegistry, ToolRuntime};
pub use tool_trait::Tool;
pub use tools::all_tools;
