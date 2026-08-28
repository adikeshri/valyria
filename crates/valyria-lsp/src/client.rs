//! The LSP client: one live conversation with one language server.
//!
//! Generic over the streams rather than tied to a child process, so the
//! whole thing — lifecycle, correlation, timeouts, crash handling — is
//! driven over an in-memory pipe in tests. Testing an LSP client only
//! against real servers means testing it only on machines that happen to
//! have them installed, which is to say barely testing it.
//!
//! Three behaviours here are load-bearing and easy to get wrong:
//!
//! - **Every request has a deadline.** A server that stops answering must
//!   degrade the caller's answer, never block it (§4.13: enrichment, never
//!   a dependency).
//! - **Server-to-client requests are answered.** Servers send requests of
//!   their own (`client/registerCapability`,
//!   `window/workDoneProgress/create`) and several of them block until
//!   answered. Ignoring these is the classic reason an LSP integration
//!   "randomly hangs on some servers".
//! - **A dead server fails its callers immediately.** When the reader task
//!   sees end of stream, every in-flight request is completed with
//!   [`LspError::ServerGone`] rather than left to time out one by one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::error::{LspError, Result};
use crate::framing::{read_message, write_message};
use crate::model::{
    symbol_kind_from_lsp, Diagnostic, Location, Position, ResultSource, ServerCapabilities,
    Severity, SymbolInfo,
};
use crate::uri;

pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// The `initialize` handshake gets a longer deadline than everything else:
/// a server that indexes on startup (rust-analyzer on a cold cache) can
/// legitimately take tens of seconds to answer it.
pub const DEFAULT_INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<std::result::Result<Value, LspError>>>>>;

pub struct LspClient {
    language: String,
    root: PathBuf,
    outgoing: mpsc::UnboundedSender<Value>,
    pending: Pending,
    diagnostics: Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
    capabilities: Mutex<ServerCapabilities>,
    next_id: AtomicI64,
    request_timeout: Duration,
    tasks: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for LspClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspClient")
            .field("language", &self.language)
            .field("root", &self.root)
            .field("capabilities", &*self.capabilities.lock())
            .finish()
    }
}

impl LspClient {
    /// Start a client over an already-connected pair of streams.
    ///
    /// Two background tasks are spawned: one draining the outgoing queue
    /// into `writer`, one reading `reader` and dispatching. They stop when
    /// the client is dropped or the server's stream ends.
    pub fn connect<R, W>(
        language: impl Into<String>,
        root: impl Into<PathBuf>,
        reader: R,
        writer: W,
        request_timeout: Duration,
    ) -> Arc<Self>
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (outgoing_tx, mut outgoing_rx) = mpsc::unbounded_channel::<Value>();
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics = Arc::new(Mutex::new(HashMap::new()));

        let client = Arc::new(Self {
            language: language.into(),
            root: root.into(),
            outgoing: outgoing_tx.clone(),
            pending: pending.clone(),
            diagnostics: diagnostics.clone(),
            capabilities: Mutex::new(ServerCapabilities::default()),
            next_id: AtomicI64::new(1),
            request_timeout,
            tasks: Mutex::new(Vec::new()),
        });

        let writer_task = tokio::spawn(async move {
            let mut writer = writer;
            while let Some(message) = outgoing_rx.recv().await {
                if let Err(e) = write_message(&mut writer, &message).await {
                    tracing::debug!(error = %e, "language server stdin closed");
                    break;
                }
            }
        });

        let reader_language = client.language.clone();
        let reader_root = client.root.clone();
        let reader_task = tokio::spawn(async move {
            let mut reader = BufReader::new(reader);
            loop {
                match read_message(&mut reader).await {
                    Ok(Some(message)) => {
                        dispatch(&message, &pending, &diagnostics, &outgoing_tx, &reader_root);
                    }
                    Ok(None) => break,
                    Err(e) => {
                        // A malformed frame desynchronizes the stream:
                        // there is no way to find the next message
                        // boundary, so the session ends rather than
                        // producing garbage indefinitely.
                        tracing::warn!(language = %reader_language, error = %e, "language server protocol error; ending session");
                        break;
                    }
                }
            }

            // The server is gone. Fail every in-flight request now instead
            // of making each one wait out its own timeout.
            let waiting: Vec<_> = pending.lock().drain().map(|(_, tx)| tx).collect();
            for tx in waiting {
                let _ = tx.send(Err(LspError::ServerGone {
                    language: reader_language.clone(),
                }));
            }
        });

        client.tasks.lock().extend([writer_task, reader_task]);
        client
    }

    pub fn language(&self) -> &str {
        &self.language
    }

    pub fn capabilities(&self) -> ServerCapabilities {
        *self.capabilities.lock()
    }

    /// Whether the session is still usable. A `false` here is the pool's
    /// signal to restart the server.
    pub fn is_alive(&self) -> bool {
        !self.outgoing.is_closed()
    }

    /// The `initialize`/`initialized` handshake. Records what the server
    /// says it can do, which every later call is gated on.
    pub async fn initialize(&self, timeout: Duration) -> Result<ServerCapabilities> {
        let params = json!({
            "processId": std::process::id(),
            "rootUri": uri::path_to_uri(&self.root),
            "workspaceFolders": [{
                "uri": uri::path_to_uri(&self.root),
                "name": self.root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            }],
            "capabilities": {
                "textDocument": {
                    "synchronization": { "dynamicRegistration": false },
                    "definition": { "dynamicRegistration": false },
                    "references": { "dynamicRegistration": false },
                    "documentSymbol": {
                        "dynamicRegistration": false,
                        "hierarchicalDocumentSymbolSupport": true,
                    },
                    "publishDiagnostics": { "relatedInformation": false },
                },
                "workspace": { "workspaceFolders": true },
            },
            "clientInfo": { "name": "valyria" },
        });

        let result = self.request("initialize", params, timeout).await?;
        let capabilities = parse_capabilities(&result);
        *self.capabilities.lock() = capabilities;

        self.notify("initialized", json!({}));
        Ok(capabilities)
    }

    pub fn did_open(&self, path: &Path, language_id: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri::path_to_uri(path),
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        );
    }

    /// Full-document sync, whatever the server asked for.
    ///
    /// Incremental sync would mean tracking every buffer's edit history to
    /// produce correct ranges, and getting that wrong desynchronizes the
    /// server silently — it starts answering about a file that no longer
    /// exists. Sending the whole document costs bandwidth to a local
    /// process and cannot desynchronize.
    pub fn did_change(&self, path: &Path, version: i64, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": { "uri": uri::path_to_uri(path), "version": version },
                "contentChanges": [{ "text": text }],
            }),
        );
    }

    pub fn did_close(&self, path: &Path) {
        let uri_string = uri::path_to_uri(path);
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri_string.clone() } }),
        );
        self.diagnostics
            .lock()
            .remove(&uri::relative_to(&self.root, path));
    }

    pub async fn definition(&self, path: &Path, position: Position) -> Result<Vec<Location>> {
        if !self.capabilities().definition {
            return Ok(Vec::new());
        }
        let result = self
            .request(
                "textDocument/definition",
                self.position_params(path, position),
                self.request_timeout,
            )
            .await?;
        Ok(parse_locations(&result, &self.root))
    }

    pub async fn references(&self, path: &Path, position: Position) -> Result<Vec<Location>> {
        if !self.capabilities().references {
            return Ok(Vec::new());
        }
        let mut params = self.position_params(path, position);
        params["context"] = json!({ "includeDeclaration": true });
        let result = self
            .request("textDocument/references", params, self.request_timeout)
            .await?;
        Ok(parse_locations(&result, &self.root))
    }

    pub async fn document_symbols(&self, path: &Path) -> Result<Vec<SymbolInfo>> {
        if !self.capabilities().document_symbols {
            return Ok(Vec::new());
        }
        let result = self
            .request(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": uri::path_to_uri(path) } }),
                self.request_timeout,
            )
            .await?;

        let path_string = uri::relative_to(&self.root, path);
        let mut out = Vec::new();
        collect_symbols(&result, &path_string, None, &self.root, &mut out);
        Ok(out)
    }

    /// Diagnostics the server has pushed for this file.
    ///
    /// Read from a cache rather than requested: `publishDiagnostics` is a
    /// notification, so there is nothing to ask — the answer is whatever
    /// the server has said most recently, which may be nothing at all if
    /// it has not finished analyzing yet.
    pub fn diagnostics(&self, path: &Path) -> Vec<Diagnostic> {
        self.diagnostics
            .lock()
            .get(&uri::relative_to(&self.root, path))
            .cloned()
            .unwrap_or_default()
    }

    /// The `shutdown`/`exit` handshake.
    ///
    /// A server that does not answer `shutdown` is still sent `exit` and
    /// then abandoned: refusing to give up on a wedged server would leak
    /// the process.
    pub async fn shutdown(&self) {
        let _ = self
            .request("shutdown", Value::Null, Duration::from_secs(3))
            .await;
        self.notify("exit", Value::Null);
    }

    fn position_params(&self, path: &Path, position: Position) -> Value {
        json!({
            "textDocument": { "uri": uri::path_to_uri(path) },
            "position": { "line": position.line, "character": position.character },
        })
    }

    fn notify(&self, method: &str, params: Value) {
        let _ = self.outgoing.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
    }

    async fn request(
        &self,
        method: &'static str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(id, tx);

        if self
            .outgoing
            .send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": method,
                "params": params,
            }))
            .is_err()
        {
            self.pending.lock().remove(&id);
            return Err(LspError::ServerGone {
                language: self.language.clone(),
            });
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(result)) => result,
            // The oneshot was dropped without a reply: the reader task
            // ended between the send and now.
            Ok(Err(_)) => Err(LspError::ServerGone {
                language: self.language.clone(),
            }),
            Err(_) => {
                // Drop the correlation entry, or a slow answer arriving
                // later would accumulate forever.
                self.pending.lock().remove(&id);
                Err(LspError::Timeout {
                    method,
                    millis: timeout.as_millis() as u64,
                })
            }
        }
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        for task in self.tasks.lock().drain(..) {
            task.abort();
        }
    }
}

/// Route one incoming message: a response to a pending request, a request
/// from the server that needs an answer, or a notification.
fn dispatch(
    message: &Value,
    pending: &Pending,
    diagnostics: &Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
    outgoing: &mpsc::UnboundedSender<Value>,
    root: &Path,
) {
    let id = message.get("id");
    let method = message.get("method").and_then(|m| m.as_str());

    match (id, method) {
        // A response: has an id, no method.
        (Some(id), None) => {
            let Some(id) = id.as_i64() else { return };
            let Some(tx) = pending.lock().remove(&id) else {
                // A response to a request that already timed out. Dropping
                // it is correct — the caller has moved on.
                return;
            };
            let outcome = match message.get("error") {
                Some(error) => Err(LspError::Rejected {
                    method: "request",
                    code: error.get("code").and_then(|c| c.as_i64()).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(|m| m.as_str())
                        .unwrap_or("(no message)")
                        .to_string(),
                }),
                None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(outcome);
        }

        // A request *from* the server: has both an id and a method, and is
        // waiting for an answer. Several servers block until they get one.
        (Some(id), Some(_)) => {
            let _ = outgoing.send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": Value::Null,
            }));
        }

        // A notification.
        (None, Some(method)) => {
            if method == "textDocument/publishDiagnostics" {
                if let Some(params) = message.get("params") {
                    record_diagnostics(params, diagnostics, root);
                }
            }
        }

        (None, None) => {}
    }
}

fn record_diagnostics(
    params: &Value,
    store: &Arc<Mutex<HashMap<String, Vec<Diagnostic>>>>,
    root: &Path,
) {
    let Some(uri_string) = params.get("uri").and_then(|u| u.as_str()) else {
        return;
    };
    let Some(path) = uri::uri_to_path(uri_string) else {
        return;
    };
    let key = uri::relative_to(root, &path);

    let parsed: Vec<Diagnostic> = params
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| parse_diagnostic(item, &key))
                .collect()
        })
        .unwrap_or_default();

    // Replace rather than merge: a `publishDiagnostics` payload is the
    // complete current set for that file, and an empty one means "these
    // are all fixed now".
    store.lock().insert(key, parsed);
}

fn parse_diagnostic(value: &Value, path: &str) -> Option<Diagnostic> {
    let range = value.get("range")?;
    Some(Diagnostic {
        location: Location {
            path: path.to_string(),
            start: parse_position(range.get("start")?)?,
            end: parse_position(range.get("end")?)?,
        },
        severity: value
            .get("severity")
            .and_then(|s| s.as_i64())
            .map(Severity::from_lsp)
            .unwrap_or(Severity::Warning),
        code: value.get("code").map(|c| match c {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
        source: value
            .get("source")
            .and_then(|s| s.as_str())
            .map(|s| s.to_string()),
        message: value
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

fn parse_position(value: &Value) -> Option<Position> {
    Some(Position {
        line: value.get("line")?.as_u64()? as u32,
        character: value.get("character")?.as_u64()? as u32,
    })
}

/// `textDocument/definition` may answer with a single `Location`, an array
/// of them, or an array of `LocationLink`s — all three are valid, and a
/// client that handles only one of them works against some servers and
/// silently returns nothing against others.
fn parse_locations(result: &Value, root: &Path) -> Vec<Location> {
    match result {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| parse_location(item, root))
            .collect(),
        Value::Object(_) => parse_location(result, root).into_iter().collect(),
        _ => Vec::new(),
    }
}

fn parse_location(value: &Value, root: &Path) -> Option<Location> {
    // `LocationLink` names the same things differently.
    let uri_string = value
        .get("uri")
        .or_else(|| value.get("targetUri"))?
        .as_str()?;
    let range = value
        .get("range")
        .or_else(|| value.get("targetSelectionRange"))
        .or_else(|| value.get("targetRange"))?;

    let path = uri::uri_to_path(uri_string)?;
    Some(Location {
        path: uri::relative_to(root, &path),
        start: parse_position(range.get("start")?)?,
        end: parse_position(range.get("end")?)?,
    })
}

/// `textDocument/documentSymbol` also has two shapes: a flat
/// `SymbolInformation[]` and a nested `DocumentSymbol[]`. The nested form
/// carries no URI (it is implicitly the requested file) and needs
/// recursion to reach members.
fn collect_symbols(
    result: &Value,
    path: &str,
    container: Option<&str>,
    root: &Path,
    out: &mut Vec<SymbolInfo>,
) {
    let Some(items) = result.as_array() else {
        return;
    };

    for item in items {
        let Some(name) = item.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let kind = symbol_kind_from_lsp(item.get("kind").and_then(|k| k.as_i64()).unwrap_or(0));

        // Flat form: the range hangs off a `location`. Nested form: it is
        // `range`/`selectionRange` directly on the symbol.
        let location = if let Some(location) = item.get("location") {
            parse_location(location, root)
        } else {
            item.get("selectionRange")
                .or_else(|| item.get("range"))
                .and_then(|range| {
                    Some(Location {
                        path: path.to_string(),
                        start: parse_position(range.get("start")?)?,
                        end: parse_position(range.get("end")?)?,
                    })
                })
        };
        let Some(location) = location else { continue };

        out.push(SymbolInfo {
            name: name.to_string(),
            kind,
            container: item
                .get("containerName")
                .and_then(|c| c.as_str())
                .map(|s| s.to_string())
                .or_else(|| container.map(|c| c.to_string())),
            location,
            source: ResultSource::LanguageServer,
        });

        if let Some(children) = item.get("children") {
            collect_symbols(children, path, Some(name), root, out);
        }
    }
}

fn parse_capabilities(result: &Value) -> ServerCapabilities {
    let caps = result.get("capabilities").unwrap_or(&Value::Null);

    // A provider field is `true`, or an options object, or absent. All
    // three are legal, and treating an options object as "not supported"
    // would silently disable half the features on servers that use them.
    let supported = |name: &str| -> bool {
        match caps.get(name) {
            Some(Value::Bool(value)) => *value,
            Some(Value::Object(_)) => true,
            _ => false,
        }
    };

    let sync = match caps.get("textDocumentSync") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(1) as u8,
        Some(Value::Object(options)) => {
            options.get("change").and_then(|c| c.as_u64()).unwrap_or(1) as u8
        }
        _ => 1,
    };

    ServerCapabilities {
        definition: supported("definitionProvider"),
        references: supported("referencesProvider"),
        document_symbols: supported("documentSymbolProvider"),
        hover: supported("hoverProvider"),
        rename: supported("renameProvider"),
        text_document_sync: sync,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_accept_both_the_boolean_and_the_options_object_form() {
        let result = json!({
            "capabilities": {
                "definitionProvider": true,
                "referencesProvider": { "workDoneProgress": true },
                "documentSymbolProvider": false,
                "textDocumentSync": { "openClose": true, "change": 2 },
            }
        });
        let caps = parse_capabilities(&result);
        assert!(caps.definition);
        assert!(caps.references, "an options object means supported");
        assert!(!caps.document_symbols);
        assert_eq!(caps.text_document_sync, 2);
    }

    #[test]
    fn an_absent_capability_is_unsupported() {
        let caps = parse_capabilities(&json!({ "capabilities": {} }));
        assert!(!caps.definition);
        assert!(!caps.rename);
    }

    #[test]
    fn a_result_with_no_capabilities_at_all_does_not_panic() {
        assert_eq!(
            parse_capabilities(&Value::Null),
            ServerCapabilities {
                text_document_sync: 1,
                ..Default::default()
            }
        );
    }

    #[test]
    fn definitions_parse_from_a_single_location() {
        let root = Path::new("/repo");
        let result = json!({
            "uri": "file:///repo/src/lib.rs",
            "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 9 } }
        });
        let locations = parse_locations(&result, root);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, "src/lib.rs");
        assert_eq!(locations[0].start.line, 3);
    }

    #[test]
    fn definitions_parse_from_a_location_link_array() {
        // `LocationLink` names the same fields differently; a client that
        // only knows `Location` silently returns nothing here.
        let root = Path::new("/repo");
        let result = json!([{
            "targetUri": "file:///repo/src/lib.rs",
            "targetSelectionRange": {
                "start": { "line": 1, "character": 0 },
                "end": { "line": 1, "character": 5 }
            }
        }]);
        let locations = parse_locations(&result, root);
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].path, "src/lib.rs");
    }

    #[test]
    fn a_null_definition_result_is_an_empty_list() {
        assert!(parse_locations(&Value::Null, Path::new("/repo")).is_empty());
    }

    #[test]
    fn a_definition_outside_the_workspace_keeps_its_absolute_path() {
        let root = Path::new("/repo");
        let result = json!({
            "uri": "file:///home/.cargo/registry/serde/lib.rs",
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } }
        });
        let locations = parse_locations(&result, root);
        assert_eq!(locations[0].path, "/home/.cargo/registry/serde/lib.rs");
    }

    #[test]
    fn nested_document_symbols_are_flattened_with_their_container() {
        let result = json!([{
            "name": "Parser",
            "kind": 5,
            "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 9, "character": 0 } },
            "selectionRange": { "start": { "line": 0, "character": 6 }, "end": { "line": 0, "character": 12 } },
            "children": [{
                "name": "parse",
                "kind": 6,
                "range": { "start": { "line": 1, "character": 2 }, "end": { "line": 3, "character": 2 } },
                "selectionRange": { "start": { "line": 1, "character": 5 }, "end": { "line": 1, "character": 10 } }
            }]
        }]);

        let mut out = Vec::new();
        collect_symbols(&result, "src/lib.rs", None, Path::new("/repo"), &mut out);

        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "Parser");
        assert_eq!(out[1].name, "parse");
        assert_eq!(out[1].container.as_deref(), Some("Parser"));
        assert_eq!(out[1].source, ResultSource::LanguageServer);
    }

    #[test]
    fn flat_document_symbols_are_parsed_too() {
        let result = json!([{
            "name": "helper",
            "kind": 12,
            "containerName": "mod",
            "location": {
                "uri": "file:///repo/src/lib.rs",
                "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 7, "character": 1 } }
            }
        }]);

        let mut out = Vec::new();
        collect_symbols(&result, "src/lib.rs", None, Path::new("/repo"), &mut out);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "helper");
        assert_eq!(out[0].container.as_deref(), Some("mod"));
    }

    #[test]
    fn a_symbol_with_no_usable_range_is_skipped_not_faked() {
        let result = json!([{ "name": "broken", "kind": 12 }]);
        let mut out = Vec::new();
        collect_symbols(&result, "src/lib.rs", None, Path::new("/repo"), &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn diagnostics_carry_their_rule_code_whether_it_is_a_string_or_a_number() {
        let string_code = parse_diagnostic(
            &json!({
                "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1} },
                "severity": 1,
                "code": "E0308",
                "source": "rustc",
                "message": "mismatched types"
            }),
            "src/lib.rs",
        )
        .unwrap();
        assert_eq!(string_code.code.as_deref(), Some("E0308"));
        assert_eq!(string_code.severity, Severity::Error);

        let numeric_code = parse_diagnostic(
            &json!({
                "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1} },
                "code": 2304,
                "message": "cannot find name"
            }),
            "src/lib.rs",
        )
        .unwrap();
        assert_eq!(numeric_code.code.as_deref(), Some("2304"));
    }
}
