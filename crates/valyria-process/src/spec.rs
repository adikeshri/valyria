//! The command specification the runner executes. Deliberately argv-based
//! (`program` + `args`), never a shell string — §20's "command validation"
//! starts with not having a shell parse anything by default.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    /// Must already be validated by the caller (e.g. via
    /// `WorkspaceRoot::resolve`) — this crate does not know about
    /// workspace roots (layering) and trusts the caller to have restricted
    /// the working directory appropriately (§20 "working-directory
    /// restrictions").
    pub cwd: PathBuf,
    pub env: HashMap<String, String>,
    pub timeout: Option<Duration>,
    pub idle_timeout: Option<Duration>,
    pub max_output_bytes: usize,
}

pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 1_000_000; // 1 MB per stream

impl CommandSpec {
    pub fn new(program: impl Into<String>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: HashMap::new(),
            timeout: Some(Duration::from_secs(120)),
            idle_timeout: None,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, vars: HashMap<String, String>) -> Self {
        self.env = vars;
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn no_timeout(mut self) -> Self {
        self.timeout = None;
        self
    }

    pub fn idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    pub fn max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes;
        self
    }
}
