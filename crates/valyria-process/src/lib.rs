//! `valyria-process` — layer 1 (Platform).
//!
//! The shell runtime (§20): argv-based command execution (never a raw
//! shell string), process-group spawn and kill, streamed output under
//! head/tail caps, wall-clock and idle timeouts, cancellation, and
//! allowlist-first environment construction. Working-directory
//! restriction is the caller's responsibility (typically enforced via
//! `valyria-vfs::WorkspaceRoot` before a `CommandSpec` is even built) —
//! this crate does not know about workspace roots.

// Not `forbid(unsafe_code)`: `runner::kill_process_group` makes one
// `libc::killpg` FFI call on unix, justified with a `SAFETY` comment at
// the call site. Everything else in this crate is safe.

pub mod env_policy;
pub mod error;
pub mod output_cap;
pub mod runner;
pub mod spec;

pub use env_policy::EnvPolicy;
pub use error::{ProcessError, Result};
pub use output_cap::{CappedOutput, CapturedOutput};
pub use runner::{run, EndReason, ExecutionResult};
pub use spec::{CommandSpec, DEFAULT_MAX_OUTPUT_BYTES};
