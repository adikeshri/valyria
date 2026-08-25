//! A sandbox profile: what a confined command is allowed to touch. This is
//! the shape every platform launcher translates into its own native
//! mechanism (a Seatbelt profile on macOS, namespaces/landlock on Linux,
//! a job object/AppContainer on Windows).

use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct SandboxProfile {
    /// Paths a confined command may write to. Filesystem reads are, by
    /// design, not restricted to an explicit allowlist — a real program
    /// (even something as simple as `echo`) needs to read a large,
    /// version-dependent set of system libraries and framework paths just
    /// to start up, and enumerating that precisely is fragile across OS
    /// updates. Writes are where the actual damage happens (§25, §49), so
    /// that's what's allowlisted.
    pub allow_write: Vec<PathBuf>,
    pub allow_network: bool,
}

impl SandboxProfile {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allow_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.allow_write.push(path.into());
        self
    }

    pub fn allow_network(mut self, allow: bool) -> Self {
        self.allow_network = allow;
        self
    }
}
