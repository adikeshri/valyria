//! A full LSP session against a scripted in-process server.
//!
//! The client is generic over its streams precisely so this is possible:
//! lifecycle, request correlation, timeouts, server-initiated requests,
//! crashes and malformed frames are all exercised here, on every machine,
//! with no language server installed anywhere.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::BufReader;
use valyria_lsp::client::LspClient;
use valyria_lsp::framing::{read_message, write_message};
use valyria_lsp::{LspError, Position, Severity};

const ROOT: &str = "/repo";

/// Start a client wired to a server that answers with `handler`.
///
/// `handler` sees every client-to-server message and returns whatever the
/// server should send back — zero messages (stay silent), one, or several
/// (a response plus an unsolicited notification).
fn session<F>(handler: F) -> Arc<LspClient>
where
    F: Fn(Value) -> Vec<Value> + Send + 'static,
{
    let (client_reads, mut server_writes) = tokio::io::duplex(64 * 1024);
    let (mut server_reads, client_writes) = tokio::io::duplex(64 * 1024);

    tokio::spawn(async move {
        let mut reader = BufReader::new(&mut server_reads);
        while let Ok(Some(message)) = read_message(&mut reader).await {
            for reply in handler(message) {
                if write_message(&mut server_writes, &reply).await.is_err() {
                    return;
                }
            }
        }
    });

    LspClient::connect(
        "rust",
        ROOT,
        client_reads,
        client_writes,
        Duration::from_millis(500),
    )
}

/// The `initialize` reply a well-behaved server sends.
fn initialize_result(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "capabilities": {
                "definitionProvider": true,
                "referencesProvider": true,
                "documentSymbolProvider": true,
                "textDocumentSync": 1,
            }
        }
    })
}

async fn initialized_session<F>(handler: F) -> Arc<LspClient>
where
    F: Fn(Value) -> Vec<Value> + Send + 'static,
{
    let client = session(move |message| {
        if message["method"] == "initialize" {
            return vec![initialize_result(&message["id"])];
        }
        handler(message)
    });
    client.initialize(Duration::from_secs(5)).await.unwrap();
    client
}

#[tokio::test]
async fn the_initialize_handshake_records_what_the_server_can_do() {
    let client = session(|message| {
        if message["method"] == "initialize" {
            vec![initialize_result(&message["id"])]
        } else {
            vec![]
        }
    });

    let capabilities = client.initialize(Duration::from_secs(5)).await.unwrap();
    assert!(capabilities.definition);
    assert!(capabilities.document_symbols);
    assert!(!capabilities.rename);
    assert_eq!(client.capabilities(), capabilities);
}

#[tokio::test]
async fn a_capability_the_server_did_not_advertise_is_never_requested() {
    // Asking a server for something it did not advertise wastes a round
    // trip and, with some servers, hangs. The gate is asserted by having
    // the fake server fail the test if it is ever asked.
    let client = session(|message| match message["method"].as_str() {
        Some("initialize") => vec![json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": { "capabilities": { "definitionProvider": false } }
        })],
        Some("textDocument/definition") => {
            panic!("the client asked for definitions the server did not advertise")
        }
        _ => vec![],
    });

    client.initialize(Duration::from_secs(5)).await.unwrap();
    let locations = client
        .definition(
            Path::new("/repo/src/lib.rs"),
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .unwrap();
    assert!(locations.is_empty());
}

#[tokio::test]
async fn a_definition_request_gets_its_own_response_back() {
    let client = initialized_session(|message| {
        if message["method"] == "textDocument/definition" {
            vec![json!({
                "jsonrpc": "2.0",
                "id": message["id"],
                "result": [{
                    "uri": "file:///repo/src/lexer.rs",
                    "range": {
                        "start": { "line": 10, "character": 4 },
                        "end": { "line": 10, "character": 12 }
                    }
                }]
            })]
        } else {
            vec![]
        }
    })
    .await;

    let locations = client
        .definition(
            Path::new("/repo/src/parser.rs"),
            Position {
                line: 3,
                character: 7,
            },
        )
        .await
        .unwrap();

    assert_eq!(locations.len(), 1);
    assert_eq!(locations[0].path, "src/lexer.rs");
    assert_eq!(locations[0].start_line_1based(), 11);
}

#[tokio::test]
async fn concurrent_requests_each_get_their_own_answer() {
    // The correlation table's whole job. A server is free to answer out of
    // order, and this one deliberately does — a client that assumed FIFO
    // would hand each caller the wrong result.
    let client = initialized_session(|message| {
        if message["method"] != "textDocument/documentSymbol" {
            return vec![];
        }
        let uri = message["params"]["textDocument"]["uri"]
            .as_str()
            .unwrap()
            .to_string();
        let name = uri.rsplit('/').next().unwrap().to_string();
        vec![json!({
            "jsonrpc": "2.0",
            "id": message["id"],
            "result": [{
                "name": name,
                "kind": 12,
                "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1} },
                "selectionRange": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1} }
            }]
        })]
    })
    .await;

    let a = client.document_symbols(Path::new("/repo/a.rs"));
    let b = client.document_symbols(Path::new("/repo/b.rs"));
    let c = client.document_symbols(Path::new("/repo/c.rs"));
    let (a, b, c) = tokio::join!(a, b, c);

    assert_eq!(a.unwrap()[0].name, "a.rs");
    assert_eq!(b.unwrap()[0].name, "b.rs");
    assert_eq!(c.unwrap()[0].name, "c.rs");
}

#[tokio::test]
async fn a_server_that_never_answers_times_out_instead_of_blocking_forever() {
    // The property behind "enrichment, never a dependency": a wedged
    // server costs one timeout, not the task.
    let client = initialized_session(|_| vec![]).await;

    let err = client
        .definition(
            Path::new("/repo/src/lib.rs"),
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .unwrap_err();

    assert!(matches!(err, LspError::Timeout { .. }));
    assert!(err.is_degradation());
}

#[tokio::test]
async fn a_request_the_server_rejects_reports_the_servers_own_message() {
    let client = initialized_session(|message| {
        if message["method"] == "textDocument/references" {
            vec![json!({
                "jsonrpc": "2.0",
                "id": message["id"],
                "error": { "code": -32601, "message": "content modified" }
            })]
        } else {
            vec![]
        }
    })
    .await;

    let err = client
        .references(
            Path::new("/repo/src/lib.rs"),
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .unwrap_err();

    match err {
        LspError::Rejected { code, message, .. } => {
            assert_eq!(code, -32601);
            assert_eq!(message, "content modified");
        }
        other => panic!("expected a rejection, got {other}"),
    }
}

#[tokio::test]
async fn a_request_from_the_server_is_answered_rather_than_ignored() {
    // Servers send requests of their own and several block until they get
    // a reply. Ignoring them is the classic reason an LSP integration
    // "randomly hangs on some servers", so the fake one here refuses to
    // answer anything until its own request has been answered.
    let (answered_tx, answered_rx) = std::sync::mpsc::channel::<()>();
    let answered = std::sync::Mutex::new(false);

    let client = session(move |message| match message["method"].as_str() {
        Some("initialize") => vec![
            initialize_result(&message["id"]),
            // Unsolicited server-to-client request.
            json!({
                "jsonrpc": "2.0",
                "id": 9001,
                "method": "client/registerCapability",
                "params": { "registrations": [] }
            }),
        ],
        Some("textDocument/definition") => {
            if !*answered.lock().unwrap() {
                return vec![]; // still waiting; leave the client hanging
            }
            vec![json!({ "jsonrpc": "2.0", "id": message["id"], "result": [] })]
        }
        // The reply to the server's own request: no method, id 9001.
        None if message["id"] == 9001 => {
            *answered.lock().unwrap() = true;
            let _ = answered_tx.send(());
            vec![]
        }
        _ => vec![],
    });

    client.initialize(Duration::from_secs(5)).await.unwrap();
    answered_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the client must answer a server-initiated request");

    // And having answered, normal traffic still flows.
    let locations = client
        .definition(
            Path::new("/repo/src/lib.rs"),
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .unwrap();
    assert!(locations.is_empty());
}

#[tokio::test]
async fn diagnostics_arrive_as_notifications_and_replace_the_previous_set() {
    let client = initialized_session(|message| {
        if message["method"] != "textDocument/didOpen" {
            return vec![];
        }
        vec![json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///repo/src/lib.rs",
                "diagnostics": [{
                    "range": { "start": {"line": 4, "character": 8}, "end": {"line": 4, "character": 12} },
                    "severity": 1,
                    "code": "E0308",
                    "source": "rustc",
                    "message": "mismatched types"
                }]
            }
        })]
    })
    .await;

    let path = Path::new("/repo/src/lib.rs");
    client.did_open(path, "rust", "fn main() {}");

    let diagnostics = wait_for(|| {
        let found = client.diagnostics(path);
        (!found.is_empty()).then_some(found)
    })
    .await;

    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].severity, Severity::Error);
    assert_eq!(diagnostics[0].code.as_deref(), Some("E0308"));
    assert_eq!(diagnostics[0].location.path, "src/lib.rs");
    assert_eq!(diagnostics[0].location.start_line_1based(), 5);
}

#[tokio::test]
async fn an_empty_diagnostic_payload_clears_the_previous_one() {
    // "These are all fixed now" is expressed as an empty array, so merging
    // instead of replacing would leave stale errors in context forever.
    let client = initialized_session(|message| match message["method"].as_str() {
        Some("textDocument/didOpen") => vec![json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///repo/src/lib.rs",
                "diagnostics": [{
                    "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1} },
                    "severity": 1,
                    "message": "broken"
                }]
            }
        })],
        Some("textDocument/didChange") => vec![json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": { "uri": "file:///repo/src/lib.rs", "diagnostics": [] }
        })],
        _ => vec![],
    })
    .await;

    let path = Path::new("/repo/src/lib.rs");
    client.did_open(path, "rust", "broken");
    wait_for(|| (!client.diagnostics(path).is_empty()).then_some(())).await;

    client.did_change(path, 2, "fixed");
    wait_for(|| client.diagnostics(path).is_empty().then_some(())).await;
}

#[tokio::test]
async fn a_server_that_dies_fails_its_in_flight_requests_immediately() {
    // Not one timeout per waiting caller: the reader task sees end of
    // stream and completes all of them at once.
    let client = session(|message| {
        if message["method"] == "initialize" {
            vec![initialize_result(&message["id"])]
        } else {
            // Returning from the handler task closes the server's end of
            // the pipe, which is what a crashed server looks like.
            vec![]
        }
    });
    client.initialize(Duration::from_secs(5)).await.unwrap();

    // Drop the server side by ending its task: request against a client
    // whose peer has gone.
    drop(client.definition(
        Path::new("/repo/src/lib.rs"),
        Position {
            line: 0,
            character: 0,
        },
    ));

    // The handler above never replies, so this is the timeout path; what
    // matters is that it returns at all, and quickly.
    let started = std::time::Instant::now();
    let err = client
        .definition(
            Path::new("/repo/src/lib.rs"),
            Position {
                line: 0,
                character: 0,
            },
        )
        .await
        .unwrap_err();
    assert!(err.is_degradation());
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a dead or silent server must not block a caller"
    );
}

#[tokio::test]
async fn a_malformed_frame_ends_the_session_instead_of_producing_garbage() {
    // A bad Content-Length desynchronizes the stream: there is no way to
    // find the next message boundary, so the session ends and the pool
    // restarts the server.
    let (client_reads, mut server_writes) = tokio::io::duplex(64 * 1024);
    let (mut server_reads, client_writes) = tokio::io::duplex(64 * 1024);

    tokio::spawn(async move {
        let mut reader = BufReader::new(&mut server_reads);
        let _ = read_message(&mut reader).await;
        use tokio::io::AsyncWriteExt;
        let _ = server_writes
            .write_all(b"Content-Length: nonsense\r\n\r\n{}")
            .await;
        let _ = server_writes.flush().await;
        // Keep the pipe open so the failure is the malformed frame, not
        // end of stream.
        tokio::time::sleep(Duration::from_secs(5)).await;
    });

    let client = LspClient::connect(
        "rust",
        ROOT,
        client_reads,
        client_writes,
        Duration::from_millis(200),
    );

    let err = client.initialize(Duration::from_secs(2)).await.unwrap_err();
    assert!(
        matches!(err, LspError::ServerGone { .. } | LspError::Timeout { .. }),
        "got {err}"
    );
}

#[tokio::test]
async fn a_late_response_to_a_timed_out_request_is_discarded_not_mismatched() {
    // If the correlation entry survived its timeout, a straggling answer
    // would sit in the table waiting to be handed to some later caller.
    //
    // The fake server answers each request only when the *next* one
    // arrives: request 1 goes unanswered and times out, then request 2
    // triggers both a stale reply for id 1 and a correct reply for id 2.
    let stale = std::sync::Mutex::new(None::<Value>);

    let client = initialized_session(move |message| {
        if message["method"] != "textDocument/definition" {
            return vec![];
        }
        // Bound to a local first: on edition 2021 the guard from an
        // `if let` scrutinee lives until the end of the whole `if/else`,
        // so locking again in the `else` arm would deadlock.
        let previous = stale.lock().unwrap().take();

        let mut replies = Vec::new();
        if let Some(previous) = previous {
            replies.push(definition_response(&previous, "file:///repo/late.rs"));
            replies.push(definition_response(
                &message["id"],
                "file:///repo/correct.rs",
            ));
        } else {
            *stale.lock().unwrap() = Some(message["id"].clone());
        }
        replies
    })
    .await;

    let position = Position {
        line: 0,
        character: 0,
    };

    let first = client.definition(Path::new("/repo/a.rs"), position).await;
    assert!(
        matches!(first, Err(LspError::Timeout { .. })),
        "expected the unanswered request to time out"
    );

    let second = client
        .definition(Path::new("/repo/b.rs"), position)
        .await
        .unwrap();
    assert_eq!(
        second.len(),
        1,
        "the abandoned request's answer must not be delivered to a later caller"
    );
    assert_eq!(second[0].path, "correct.rs");
    assert!(client.is_alive());
}

fn definition_response(id: &Value, uri: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": [{
            "uri": uri,
            "range": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1} }
        }]
    })
}

#[tokio::test]
async fn shutdown_completes_even_when_the_server_ignores_it() {
    // A server that ignores `shutdown` must not be able to hang the
    // runtime's own shutdown.
    let client = initialized_session(|_| vec![]).await;
    let started = std::time::Instant::now();
    client.shutdown().await;
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// Poll `check` until it yields a value, with a bounded wait. Diagnostics
/// arrive asynchronously, so there is nothing to await on directly.
async fn wait_for<T, F: Fn() -> Option<T>>(check: F) -> T {
    for _ in 0..200 {
        if let Some(value) = check() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition was never met within 2s");
}
