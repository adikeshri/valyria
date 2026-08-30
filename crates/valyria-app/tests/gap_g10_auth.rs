//! Integration coverage for local client authentication added in protocol
//! 1.6.0 (CORE-INTERFACE gap G10): the daemon peer-uid-checks every
//! connection and, when started with a token, requires every frame to
//! carry it.
//!
//! The peer-uid check's *rejection* path needs a second OS user and is not
//! exercised here; the *acceptance* path (same user) is covered by every
//! test below simply connecting.

#![cfg(unix)]

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use valyria_app::{serve, Runtime, RuntimeConfig};
use valyria_protocol::transport::SocketClient;
use valyria_protocol::{Client, HelloRequest, Request, Response};
use valyria_util::CancellationToken;

async fn test_runtime() -> Arc<Runtime> {
    let dir = tempfile::tempdir().unwrap();
    let ws = valyria_testkit::TempWorkspace::new();
    let cfg = RuntimeConfig::new(ws.path()).with_data_dir(dir.path().join("data"));
    std::mem::forget((dir, ws));
    Arc::new(Runtime::open(cfg).await.unwrap())
}

async fn spawn(
    token: Option<String>,
    suffix: &str,
) -> (
    std::path::PathBuf,
    CancellationToken,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let rt = test_runtime().await;
    let sock =
        std::env::temp_dir().join(format!("valyria-auth-{}-{suffix}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock);
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(serve(rt, sock.clone(), shutdown.clone(), token));
    for _ in 0..200 {
        if sock.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    (sock, shutdown, handle)
}

async fn hello(client: &SocketClient) -> Response {
    client
        .call(Request::Hello(HelloRequest {
            client_name: "test".into(),
        }))
        .await
}

#[tokio::test]
async fn no_token_daemon_accepts_a_plain_client() {
    let (sock, shutdown, handle) = spawn(None, "plain").await;
    match hello(&SocketClient::new(&sock)).await {
        Response::Hello(h) => assert_eq!(h.protocol_version, valyria_protocol::PROTOCOL_VERSION),
        other => panic!("expected Hello, got {other:?}"),
    }
    shutdown.cancel();
    let _ = handle.await;
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn token_daemon_accepts_the_matching_token() {
    let (sock, shutdown, handle) = spawn(Some("s3cr3t".into()), "match").await;
    match hello(&SocketClient::with_token(&sock, "s3cr3t")).await {
        Response::Hello(_) => {}
        other => panic!("expected Hello, got {other:?}"),
    }
    shutdown.cancel();
    let _ = handle.await;
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn token_daemon_rejects_a_client_with_no_token() {
    let (sock, shutdown, handle) = spawn(Some("s3cr3t".into()), "notoken").await;
    match hello(&SocketClient::new(&sock)).await {
        Response::Error(e) => assert_eq!(e.code, "auth.required"),
        other => panic!("expected auth.required, got {other:?}"),
    }
    shutdown.cancel();
    let _ = handle.await;
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn token_daemon_rejects_a_wrong_token() {
    let (sock, shutdown, handle) = spawn(Some("s3cr3t".into()), "wrong").await;
    match hello(&SocketClient::with_token(&sock, "nope")).await {
        Response::Error(e) => assert_eq!(e.code, "auth.token_mismatch"),
        other => panic!("expected auth.token_mismatch, got {other:?}"),
    }
    shutdown.cancel();
    let _ = handle.await;
    let _ = std::fs::remove_file(&sock);
}

#[tokio::test]
async fn token_daemon_authenticates_the_event_subscription() {
    let (sock, shutdown, handle) = spawn(Some("s3cr3t".into()), "sub").await;

    // A tokened client can create a task and see its events.
    let client = SocketClient::with_token(&sock, "s3cr3t");
    let task_id = match client
        .call(Request::TaskCreate(valyria_protocol::TaskCreateRequest {
            objective: "add a function".into(),
            permission_mode: Some("autonomous".into()),
        }))
        .await
    {
        Response::TaskCreate(r) => r.task_id,
        other => panic!("expected TaskCreate, got {other:?}"),
    };
    let mut events = client.subscribe_events(0).await;
    let mut saw = false;
    for _ in 0..500 {
        match tokio::time::timeout(Duration::from_secs(5), events.next()).await {
            Ok(Some(ev)) if ev.task_id.as_deref() == Some(task_id.as_str()) => {
                saw = true;
                break;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    assert!(saw, "a tokened subscription must receive events");

    // A tokenless subscription gets nothing (the daemon closes it with an error).
    let mut denied = SocketClient::new(&sock).subscribe_events(0).await;
    assert!(
        tokio::time::timeout(Duration::from_secs(2), denied.next())
            .await
            .unwrap_or(None)
            .is_none(),
        "a tokenless subscription must not stream events"
    );

    shutdown.cancel();
    let _ = handle.await;
    let _ = std::fs::remove_file(&sock);
}
