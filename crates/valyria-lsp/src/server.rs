//! Which server to run for which language, and how to start it.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use valyria_process::EnvPolicy;

use crate::client::{LspClient, DEFAULT_REQUEST_TIMEOUT};
use crate::error::{LspError, Result};

/// How to launch one language server.
#[derive(Debug, Clone)]
pub struct ServerSpec {
    /// The `valyria-lang` language id this server serves.
    pub language: &'static str,
    pub program: &'static str,
    pub args: &'static [&'static str],
    /// The LSP `languageId` to send in `didOpen`, which is not always the
    /// same string the runtime uses internally (`tsx` is `typescriptreact`
    /// to a TypeScript server).
    pub lsp_language_id: &'static str,
}

/// The servers the runtime knows how to start.
///
/// A short, curated list of the defaults for each tier-1 language rather
/// than an exhaustive one. Nothing here is required: a language with no
/// entry, or an entry whose binary is not installed, simply gets
/// index-derived results, which is the whole point of LSP being
/// enrichment (§4.13).
pub const DEFAULT_SERVERS: &[ServerSpec] = &[
    ServerSpec {
        language: "rust",
        program: "rust-analyzer",
        args: &[],
        lsp_language_id: "rust",
    },
    ServerSpec {
        language: "python",
        program: "pyright-langserver",
        args: &["--stdio"],
        lsp_language_id: "python",
    },
    ServerSpec {
        language: "go",
        program: "gopls",
        args: &[],
        lsp_language_id: "go",
    },
    ServerSpec {
        language: "typescript",
        program: "typescript-language-server",
        args: &["--stdio"],
        lsp_language_id: "typescript",
    },
    ServerSpec {
        language: "tsx",
        program: "typescript-language-server",
        args: &["--stdio"],
        lsp_language_id: "typescriptreact",
    },
    ServerSpec {
        language: "javascript",
        program: "typescript-language-server",
        args: &["--stdio"],
        lsp_language_id: "javascript",
    },
    ServerSpec {
        language: "java",
        program: "jdtls",
        args: &[],
        lsp_language_id: "java",
    },
];

pub fn spec_for(language: &str) -> Option<&'static ServerSpec> {
    DEFAULT_SERVERS.iter().find(|s| s.language == language)
}

/// A running server process and the client talking to it.
#[derive(Debug)]
pub struct RunningServer {
    pub client: std::sync::Arc<LspClient>,
    child: Child,
}

impl RunningServer {
    /// Terminate the process.
    ///
    /// Called after the `shutdown`/`exit` handshake has had its chance; a
    /// server that ignored both is killed rather than leaked. §20's "no
    /// orphan process groups" applies to language servers exactly as it
    /// does to build commands.
    pub async fn terminate(mut self) {
        self.client.shutdown().await;
        // Give the process a moment to act on `exit` before killing it.
        let graceful = tokio::time::timeout(Duration::from_millis(500), self.child.wait()).await;
        if graceful.is_err() {
            let _ = self.child.kill().await;
        }
    }
}

/// Start a language server as a child process and hand back a connected
/// client.
///
/// A missing binary comes back as [`LspError::NotInstalled`] rather than a
/// raw `NotFound` io error, because on most machines it is the expected
/// outcome for most languages, and the caller's response to it (carry on
/// without LSP) is different from its response to a real failure.
pub async fn spawn(
    spec: &ServerSpec,
    root: &Path,
    request_timeout: Duration,
) -> Result<RunningServer> {
    // §21 credential isolation applies to a language server as much as to
    // any other subprocess: it is a third-party binary reading the
    // repository, and it has no business inheriting `AWS_*` or a token.
    let inherited: std::collections::HashMap<String, String> = std::env::vars().collect();
    let env = EnvPolicy::inherit_filtered().build(&inherited);

    let mut command = Command::new(spec.program);
    command
        .args(spec.args)
        .current_dir(root)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        // Servers are chatty on stderr and nothing here reads it; letting
        // it inherit would interleave the server's logs into the
        // runtime's own output.
        .stderr(Stdio::null())
        .kill_on_drop(true);

    let mut child = command.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            LspError::NotInstalled {
                language: spec.language.to_string(),
                program: spec.program.to_string(),
            }
        } else {
            LspError::Spawn {
                language: spec.language.to_string(),
                source: e,
            }
        }
    })?;

    let stdout: ChildStdout = child.stdout.take().ok_or_else(|| LspError::Spawn {
        language: spec.language.to_string(),
        source: std::io::Error::other("child has no stdout"),
    })?;
    let stdin: ChildStdin = child.stdin.take().ok_or_else(|| LspError::Spawn {
        language: spec.language.to_string(),
        source: std::io::Error::other("child has no stdin"),
    })?;

    let client = LspClient::connect(spec.language, root, stdout, stdin, request_timeout);
    Ok(RunningServer { client, child })
}

pub fn default_request_timeout() -> Duration {
    DEFAULT_REQUEST_TIMEOUT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tier_one_language_has_a_server_or_deliberately_has_none() {
        // Not an assertion that all of them are covered — it is a
        // reminder that the *set* is deliberate. A language missing from
        // here degrades to index-only results, which is a legitimate
        // choice, not an oversight.
        for language in [
            "rust",
            "python",
            "go",
            "typescript",
            "tsx",
            "javascript",
            "java",
        ] {
            assert!(
                spec_for(language).is_some(),
                "no default server for tier-1 language `{language}`"
            );
        }
    }

    #[test]
    fn an_unknown_language_has_no_server() {
        assert!(spec_for("cobol").is_none());
    }

    #[test]
    fn tsx_sends_the_language_id_a_typescript_server_expects() {
        // The runtime calls it `tsx`; the server would not recognize that.
        assert_eq!(spec_for("tsx").unwrap().lsp_language_id, "typescriptreact");
    }

    #[tokio::test]
    async fn a_missing_binary_is_reported_as_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let spec = ServerSpec {
            language: "nonexistent",
            program: "valyria-no-such-language-server",
            args: &[],
            lsp_language_id: "nonexistent",
        };

        let err = spawn(&spec, dir.path(), Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(matches!(err, LspError::NotInstalled { .. }));
        assert!(err.is_degradation());
    }
}
