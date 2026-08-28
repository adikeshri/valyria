//! The server pool: lifecycle, capping, and the "never a dependency"
//! guarantee made concrete.
//!
//! A language server is started lazily, on the first request for its
//! language, and only once. If it cannot be started — not installed, does
//! not answer `initialize` — the pool records that and stops trying, so a
//! machine without `gopls` does not pay a failed process spawn on every
//! Go file it looks at.
//!
//! Every method returns something usable whatever the server does. There
//! is no path through this module on which a language server can fail an
//! agent task.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::client::{LspClient, DEFAULT_INITIALIZE_TIMEOUT, DEFAULT_REQUEST_TIMEOUT};
use crate::error::LspError;
use crate::model::{Diagnostic, Location, Position, SymbolInfo};
use crate::server::{self, RunningServer, ServerSpec};

#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// How many servers may run at once. Each is a full language
    /// toolchain in memory — rust-analyzer on a large repository is
    /// gigabytes — so an uncapped pool on a polyglot repository is a
    /// straightforward way to exhaust a laptop.
    pub max_servers: usize,
    pub request_timeout: Duration,
    pub initialize_timeout: Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_servers: 3,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            initialize_timeout: DEFAULT_INITIALIZE_TIMEOUT,
        }
    }
}

/// Why a language has no server. Recorded so `doctor` can explain the
/// difference between "not installed" and "installed but broken", which
/// call for different things from the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unavailable {
    NoServerConfigured,
    NotInstalled { program: String },
    FailedToStart { reason: String },
}

enum Slot {
    Running(RunningServer),
    Unavailable(Unavailable),
}

pub struct LspPool {
    root: PathBuf,
    config: PoolConfig,
    slots: Mutex<HashMap<String, Slot>>,
}

impl std::fmt::Debug for LspPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspPool")
            .field("root", &self.root)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl LspPool {
    pub fn new(root: impl Into<PathBuf>, config: PoolConfig) -> Self {
        Self {
            root: root.into(),
            config,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// A live client for `language`, starting one if needed.
    ///
    /// `None` means "carry on without LSP" and is a completely normal
    /// answer: no server is configured, the binary is not installed, the
    /// pool is full, or a previous attempt failed.
    pub async fn client_for(&self, language: &str) -> Option<Arc<LspClient>> {
        let mut slots = self.slots.lock().await;

        // A server that died since last time: drop the slot so the next
        // request starts a fresh one. Restart-on-crash, without a
        // supervisor loop that could spin.
        if let Some(Slot::Running(running)) = slots.get(language) {
            if running.client.is_alive() {
                return Some(running.client.clone());
            }
            tracing::info!(language, "language server died; will restart on next use");
            slots.remove(language);
        }

        if let Some(Slot::Unavailable(reason)) = slots.get(language) {
            tracing::trace!(language, ?reason, "language server unavailable");
            return None;
        }

        let Some(spec) = server::spec_for(language) else {
            slots.insert(
                language.to_string(),
                Slot::Unavailable(Unavailable::NoServerConfigured),
            );
            return None;
        };

        let running_count = slots
            .values()
            .filter(|slot| matches!(slot, Slot::Running(_)))
            .count();
        if running_count >= self.config.max_servers {
            // Deliberately not recorded as unavailable: the cap is a
            // transient condition, and the next request after a server
            // shuts down should be able to start this one.
            tracing::debug!(
                language,
                max = self.config.max_servers,
                "language server pool is full"
            );
            return None;
        }

        match self.start(spec).await {
            Ok(running) => {
                let client = running.client.clone();
                slots.insert(language.to_string(), Slot::Running(running));
                Some(client)
            }
            Err(reason) => {
                slots.insert(language.to_string(), Slot::Unavailable(reason));
                None
            }
        }
    }

    async fn start(&self, spec: &ServerSpec) -> std::result::Result<RunningServer, Unavailable> {
        let running = server::spawn(spec, &self.root, self.config.request_timeout)
            .await
            .map_err(|e| match e {
                LspError::NotInstalled { program, .. } => Unavailable::NotInstalled { program },
                other => Unavailable::FailedToStart {
                    reason: other.to_string(),
                },
            })?;

        // A server that starts but never completes `initialize` is worse
        // than one that is missing: it would make every later request wait
        // out its own timeout. Fail it once, here.
        match running
            .client
            .initialize(self.config.initialize_timeout)
            .await
        {
            Ok(capabilities) => {
                tracing::info!(
                    language = spec.language,
                    program = spec.program,
                    ?capabilities,
                    "language server ready"
                );
                Ok(running)
            }
            Err(e) => {
                running.terminate().await;
                Err(Unavailable::FailedToStart {
                    reason: e.to_string(),
                })
            }
        }
    }

    /// Why `language` has no server, if it does not have one. `None` means
    /// it has one, or that nothing has asked yet.
    pub async fn unavailability(&self, language: &str) -> Option<Unavailable> {
        match self.slots.lock().await.get(language) {
            Some(Slot::Unavailable(reason)) => Some(reason.clone()),
            _ => None,
        }
    }

    pub async fn running_languages(&self) -> Vec<String> {
        let slots = self.slots.lock().await;
        let mut out: Vec<String> = slots
            .iter()
            .filter(|(_, slot)| matches!(slot, Slot::Running(_)))
            .map(|(language, _)| language.clone())
            .collect();
        out.sort();
        out
    }

    /// Open a file with the right server, so later position queries about
    /// it have something to answer from. A no-op when no server is
    /// available.
    pub async fn open(&self, language: &str, path: &Path, text: &str) {
        let Some(client) = self.client_for(language).await else {
            return;
        };
        let lsp_id = server::spec_for(language)
            .map(|spec| spec.lsp_language_id)
            .unwrap_or(language);
        client.did_open(&self.root.join(path), lsp_id, text);
    }

    /// Definitions at a position, or an empty list.
    ///
    /// Every failure — no server, timeout, crash, rejection — becomes an
    /// empty list plus a log line. That is [`LspError::is_degradation`]
    /// applied: the caller merges these with index-derived results, and
    /// having none is normal.
    pub async fn definition(
        &self,
        language: &str,
        path: &Path,
        position: Position,
    ) -> Vec<Location> {
        let Some(client) = self.client_for(language).await else {
            return Vec::new();
        };
        match client.definition(&self.root.join(path), position).await {
            Ok(locations) => locations,
            Err(e) => {
                tracing::debug!(language, error = %e, "definition lookup degraded to index-only");
                Vec::new()
            }
        }
    }

    pub async fn references(
        &self,
        language: &str,
        path: &Path,
        position: Position,
    ) -> Vec<Location> {
        let Some(client) = self.client_for(language).await else {
            return Vec::new();
        };
        match client.references(&self.root.join(path), position).await {
            Ok(locations) => locations,
            Err(e) => {
                tracing::debug!(language, error = %e, "reference lookup degraded to index-only");
                Vec::new()
            }
        }
    }

    pub async fn document_symbols(&self, language: &str, path: &Path) -> Vec<SymbolInfo> {
        let Some(client) = self.client_for(language).await else {
            return Vec::new();
        };
        match client.document_symbols(&self.root.join(path)).await {
            Ok(symbols) => symbols,
            Err(e) => {
                tracing::debug!(language, error = %e, "symbol lookup degraded to index-only");
                Vec::new()
            }
        }
    }

    pub async fn diagnostics(&self, language: &str, path: &Path) -> Vec<Diagnostic> {
        let Some(client) = self.client_for(language).await else {
            return Vec::new();
        };
        client.diagnostics(&self.root.join(path))
    }

    /// Shut every server down cleanly. Called when a workspace closes.
    pub async fn shutdown_all(&self) {
        let slots: Vec<Slot> = self.slots.lock().await.drain().map(|(_, s)| s).collect();
        for slot in slots {
            if let Slot::Running(running) = slot {
                running.terminate().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_language_with_no_configured_server_yields_nothing_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let pool = LspPool::new(dir.path(), PoolConfig::default());

        assert!(pool.client_for("cobol").await.is_none());
        assert_eq!(
            pool.unavailability("cobol").await,
            Some(Unavailable::NoServerConfigured)
        );
    }

    #[tokio::test]
    async fn every_query_degrades_to_an_empty_answer_rather_than_an_error() {
        // The property that makes LSP safe to depend on nothing: a
        // machine with no language servers installed still works, it just
        // gets index-derived results.
        let dir = tempfile::tempdir().unwrap();
        let pool = LspPool::new(dir.path(), PoolConfig::default());
        let path = Path::new("src/lib.rs");
        let position = Position {
            line: 0,
            character: 0,
        };

        assert!(pool.definition("cobol", path, position).await.is_empty());
        assert!(pool.references("cobol", path, position).await.is_empty());
        assert!(pool.document_symbols("cobol", path).await.is_empty());
        assert!(pool.diagnostics("cobol", path).await.is_empty());
        pool.open("cobol", path, "IDENTIFICATION DIVISION.").await;
    }

    #[tokio::test]
    async fn a_failed_start_is_remembered_so_it_is_not_retried_every_time() {
        let dir = tempfile::tempdir().unwrap();
        let pool = LspPool::new(dir.path(), PoolConfig::default());

        // `rust` has a configured server that is almost certainly not
        // installed in a CI container; either way, the *second* call must
        // take the recorded-unavailable path rather than spawning again.
        let first = pool.client_for("rust").await;
        if first.is_none() {
            let reason = pool.unavailability("rust").await;
            assert!(reason.is_some(), "an unavailable server must record why");
            assert!(pool.client_for("rust").await.is_none());
        }
    }

    #[tokio::test]
    async fn nothing_is_running_before_anything_is_asked_for() {
        let dir = tempfile::tempdir().unwrap();
        let pool = LspPool::new(dir.path(), PoolConfig::default());
        assert!(pool.running_languages().await.is_empty());
        pool.shutdown_all().await;
    }

    #[test]
    fn the_default_pool_is_capped() {
        assert!(PoolConfig::default().max_servers > 0);
        assert!(PoolConfig::default().max_servers <= 8);
    }
}
