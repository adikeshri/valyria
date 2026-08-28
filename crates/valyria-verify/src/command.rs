//! What a verification command *is*: an explicit argv with a semantic
//! label and a record of where it was discovered from. Never a shell
//! string — same rule as `valyria-process` (§20): nothing here is parsed
//! by a shell.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use valyria_process::{CommandSpec, DEFAULT_MAX_OUTPUT_BYTES};

/// The kind of check a command performs. Ordered loosely by how cheap and
/// how localized the signal is — `Syntax`/`Typecheck` catch a regression
/// fastest, a full `Test` run catches the most. `strategy` uses this
/// ordering when it has nothing better to go on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKind {
    /// A compile / parse check with no test execution (`cargo build`,
    /// `go build ./...`, `tsc --noEmit`).
    Build,
    /// A type checker run distinct from compilation (`mypy`, `tsc`).
    Typecheck,
    /// A linter (`clippy`, `eslint`, `ruff`, `go vet`).
    Lint,
    /// A formatter run in check mode (`cargo fmt --check`, `prettier
    /// --check`, `gofmt -l`).
    Format,
    /// A test runner (`cargo test`, `pytest`, `go test`, `npm test`).
    Test,
}

impl CommandKind {
    pub const ALL: [CommandKind; 5] = [
        CommandKind::Build,
        CommandKind::Typecheck,
        CommandKind::Lint,
        CommandKind::Format,
        CommandKind::Test,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            CommandKind::Build => "build",
            CommandKind::Typecheck => "typecheck",
            CommandKind::Lint => "lint",
            CommandKind::Format => "format",
            CommandKind::Test => "test",
        }
    }
}

impl std::fmt::Display for CommandKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Where a `VerifyCommand` came from. CI workflows are the strongest
/// source — they are the commands the project's own maintainers actually
/// run on every push — so `discovery` ranks them above a guess derived
/// from a manifest's mere presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum CommandSource {
    /// Implied by a manifest existing at all (`Cargo.toml` ⇒ `cargo
    /// test`), with no project-specific configuration behind it.
    Manifest { file: String },
    /// A named entry in a manifest's script table (`package.json`
    /// `scripts`, a `Makefile` target, a `justfile` recipe).
    Script { file: String, name: String },
    /// A line lifted from a CI workflow's `run:` step.
    CiWorkflow { file: String },
    /// A tool config file whose presence names the tool to run
    /// (`.eslintrc`, `ruff.toml`, `rustfmt.toml`).
    ConfigFile { file: String },
    /// A repo-root convention (`./test.sh`, `./verify.sh`).
    Convention,
}

impl CommandSource {
    /// A coarse 0..=100 confidence that a command from this source is the
    /// *right* command to run. Used only to order otherwise-equivalent
    /// candidates; `discovery::validate` is what actually gates execution.
    pub fn confidence(&self) -> u8 {
        match self {
            CommandSource::CiWorkflow { .. } => 90,
            CommandSource::Script { .. } => 80,
            CommandSource::Convention => 75,
            CommandSource::ConfigFile { .. } => 60,
            CommandSource::Manifest { .. } => 50,
        }
    }
}

/// One discovered, runnable check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifyCommand {
    pub kind: CommandKind,
    pub program: String,
    pub args: Vec<String>,
    pub source: CommandSource,
}

impl VerifyCommand {
    pub fn new(
        kind: CommandKind,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
        source: CommandSource,
    ) -> Self {
        Self {
            kind,
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            source,
        }
    }

    /// `"cargo test --workspace"` — for logs, evidence bodies and the
    /// completion report. Never re-parsed.
    pub fn display(&self) -> String {
        if self.args.is_empty() {
            self.program.clone()
        } else {
            format!("{} {}", self.program, self.args.join(" "))
        }
    }

    /// A stable identity for dedup: two commands that would run the same
    /// process are the same candidate regardless of which source turned
    /// them up first.
    pub fn identity(&self) -> String {
        format!("{}\u{0}{}", self.program, self.args.join("\u{0}"))
    }

    /// Build the `valyria-process` spec that runs this command in
    /// `cwd`, with `env` already constructed by the caller (allowlist-first
    /// per §21 — this crate does not touch the ambient environment).
    pub fn to_spec(
        &self,
        cwd: impl Into<PathBuf>,
        env: HashMap<String, String>,
        timeout: Duration,
    ) -> CommandSpec {
        CommandSpec::new(self.program.clone(), cwd)
            .args(self.args.clone())
            .env(env)
            .timeout(timeout)
            .max_output_bytes(DEFAULT_MAX_OUTPUT_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_joins_program_and_args() {
        let c = VerifyCommand::new(
            CommandKind::Test,
            "cargo",
            ["test", "--workspace"],
            CommandSource::Manifest {
                file: "Cargo.toml".into(),
            },
        );
        assert_eq!(c.display(), "cargo test --workspace");
    }

    #[test]
    fn display_of_bare_program() {
        let c = VerifyCommand::new(
            CommandKind::Test,
            "./test.sh",
            Vec::<String>::new(),
            CommandSource::Convention,
        );
        assert_eq!(c.display(), "./test.sh");
    }

    #[test]
    fn identity_ignores_source() {
        let a = VerifyCommand::new(
            CommandKind::Test,
            "cargo",
            ["test"],
            CommandSource::Manifest {
                file: "Cargo.toml".into(),
            },
        );
        let b = VerifyCommand::new(
            CommandKind::Test,
            "cargo",
            ["test"],
            CommandSource::CiWorkflow {
                file: ".github/workflows/ci.yml".into(),
            },
        );
        assert_eq!(a.identity(), b.identity());
    }

    #[test]
    fn ci_workflow_outranks_manifest() {
        assert!(
            (CommandSource::CiWorkflow { file: "x".into() }).confidence()
                > (CommandSource::Manifest { file: "y".into() }).confidence()
        );
    }

    #[test]
    fn kind_ordering_puts_build_before_test() {
        assert!(CommandKind::Build < CommandKind::Test);
        assert!(CommandKind::Typecheck < CommandKind::Test);
    }

    #[test]
    fn to_spec_carries_argv_and_timeout() {
        let c = VerifyCommand::new(
            CommandKind::Lint,
            "cargo",
            ["clippy", "--", "-Dwarnings"],
            CommandSource::Manifest {
                file: "Cargo.toml".into(),
            },
        );
        let spec = c.to_spec("/tmp", HashMap::new(), Duration::from_secs(30));
        assert_eq!(spec.program, "cargo");
        assert_eq!(spec.args, vec!["clippy", "--", "-Dwarnings"]);
        assert_eq!(spec.timeout, Some(Duration::from_secs(30)));
    }
}
