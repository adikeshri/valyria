//! The daemon accept-loop (§4.27, D3-alt: "Daemon vs embedded — default:
//! both"). `serve` listens on a Unix domain socket and dispatches every
//! framed [`ClientFrame`] straight into an [`EmbeddedClient`] — so a CLI
//! talking to the daemon over the socket and a CLI running the runtime
//! in-process execute *identical* code past this point. That identity is
//! the whole point of D11.
//!
//! The transport is a Unix-domain socket, so the daemon is Unix-only. On
//! other platforms [`serve`] is a stub that returns an `Unsupported`
//! error; the embedded (in-process) path — every other CLI command — is
//! unaffected.

#[cfg(unix)]
mod imp {
    use std::sync::Arc;

    use futures::StreamExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};
    use valyria_protocol::transport::{encode_line, ClientFrame, ServerFrame};
    use valyria_protocol::Client;
    use valyria_util::CancellationToken;

    use crate::client::EmbeddedClient;
    use crate::runtime::Runtime;

    /// Bind `socket_path` and serve until `shutdown` is triggered. Removes a
    /// stale socket file first (a previous run that did not clean up), and
    /// removes its own on return.
    pub async fn serve(
        runtime: Arc<Runtime>,
        socket_path: impl AsRef<std::path::Path>,
        shutdown: CancellationToken,
    ) -> std::io::Result<()> {
        let socket_path = socket_path.as_ref().to_path_buf();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        tracing::info!(path = %socket_path.display(), "valyria daemon listening");

        let client = Arc::new(EmbeddedClient::new(runtime));

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _addr)) => {
                            let client = client.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, client).await {
                                    tracing::debug!(%e, "daemon connection ended");
                                }
                            });
                        }
                        Err(e) => {
                            tracing::warn!(%e, "daemon accept failed");
                        }
                    }
                }
            }
        }

        let _ = std::fs::remove_file(&socket_path);
        Ok(())
    }

    async fn handle_connection(
        stream: UnixStream,
        client: Arc<EmbeddedClient>,
    ) -> std::io::Result<()> {
        let (read_half, mut write_half) = stream.into_split();
        let mut lines = BufReader::new(read_half).lines();

        let Some(first) = lines.next_line().await? else {
            return Ok(());
        };
        let frame: ClientFrame = match serde_json::from_str(&first) {
            Ok(f) => f,
            Err(e) => {
                let err = valyria_protocol::Response::Error(valyria_protocol::WireError {
                    code: "protocol.bad_frame".into(),
                    message: e.to_string(),
                    retryable: false,
                });
                write_half
                    .write_all(encode_line(&ServerFrame::Response(err)).as_bytes())
                    .await?;
                return Ok(());
            }
        };

        match frame {
            ClientFrame::Call(req) => {
                let resp = client.call(req).await;
                write_half
                    .write_all(encode_line(&ServerFrame::Response(resp)).as_bytes())
                    .await?;
                write_half.flush().await?;
            }
            ClientFrame::Subscribe { since } => {
                let mut events = client.subscribe_events(since).await;
                while let Some(ev) = events.next().await {
                    let line = encode_line(&ServerFrame::Event(ev));
                    if write_half.write_all(line.as_bytes()).await.is_err() {
                        break; // client hung up
                    }
                    if write_half.flush().await.is_err() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use valyria_protocol::transport::SocketClient;
        use valyria_protocol::{HelloRequest, Request, Response};

        async fn test_runtime() -> Arc<Runtime> {
            let dir = tempfile::tempdir().unwrap();
            let cfg = crate::runtime::RuntimeConfig::new(dir.path())
                .with_global_dir(dir.path().join("global"));
            let rt = Arc::new(Runtime::open(cfg).await.unwrap());
            std::mem::forget(dir);
            rt
        }

        #[tokio::test]
        async fn hello_over_the_socket_matches_the_embedded_path() {
            let rt = test_runtime().await;
            let sock =
                std::env::temp_dir().join(format!("valyria-test-{}.sock", std::process::id()));
            let shutdown = CancellationToken::new();
            let server = tokio::spawn(serve(rt.clone(), sock.clone(), shutdown.clone()));

            // Wait for the socket to appear.
            for _ in 0..100 {
                if sock.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            let client = SocketClient::new(&sock);
            let resp = client
                .call(Request::Hello(HelloRequest {
                    client_name: "test".into(),
                }))
                .await;
            match resp {
                Response::Hello(h) => {
                    assert_eq!(h.protocol_version, valyria_protocol::PROTOCOL_VERSION);
                    assert!(h.capabilities.contains(&"doctor".to_string()));
                }
                other => panic!("expected Hello, got {other:?}"),
            }

            shutdown.cancel();
            let _ = server.await;
            let _ = std::fs::remove_file(&sock);
        }

        #[tokio::test]
        async fn task_lifecycle_drives_over_the_socket() {
            let rt = test_runtime().await;
            let sock =
                std::env::temp_dir().join(format!("valyria-test-{}-b.sock", std::process::id()));
            let shutdown = CancellationToken::new();
            let server = tokio::spawn(serve(rt.clone(), sock.clone(), shutdown.clone()));
            for _ in 0..100 {
                if sock.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            let client = SocketClient::new(&sock);
            let created = client
                .call(Request::TaskCreate(valyria_protocol::TaskCreateRequest {
                    objective: "add a function".into(),
                    permission_mode: None,
                }))
                .await;
            let task_id = match created {
                Response::TaskCreate(r) => r.task_id,
                other => panic!("expected TaskCreate, got {other:?}"),
            };

            let listed = client
                .call(Request::TaskList(valyria_protocol::Empty {}))
                .await;
            match listed {
                Response::TaskList(l) => {
                    assert!(l.tasks.iter().any(|t| t.task_id == task_id));
                }
                other => panic!("expected TaskList, got {other:?}"),
            }

            shutdown.cancel();
            let _ = server.await;
            let _ = std::fs::remove_file(&sock);
        }

        #[tokio::test]
        async fn events_stream_over_the_socket_reaches_a_terminal_state() {
            let rt = test_runtime().await;
            let sock =
                std::env::temp_dir().join(format!("valyria-test-{}-c.sock", std::process::id()));
            let shutdown = CancellationToken::new();
            let server = tokio::spawn(serve(rt.clone(), sock.clone(), shutdown.clone()));
            for _ in 0..100 {
                if sock.exists() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }

            let client = SocketClient::new(&sock);
            let task_id = match client
                .call(Request::TaskCreate(valyria_protocol::TaskCreateRequest {
                    objective: "add a function".into(),
                    permission_mode: None,
                }))
                .await
            {
                Response::TaskCreate(r) => r.task_id,
                other => panic!("expected TaskCreate, got {other:?}"),
            };

            let mut events = client.subscribe_events(0).await;
            let mut saw_completed = false;
            for _ in 0..500 {
                match tokio::time::timeout(std::time::Duration::from_secs(5), events.next()).await {
                    Ok(Some(ev)) => {
                        if ev.task_id.as_deref() == Some(task_id.as_str())
                            && ev.kind == "state_changed"
                            && ev.payload.get("to").and_then(|v| v.as_str()) == Some("COMPLETED")
                        {
                            saw_completed = true;
                            break;
                        }
                    }
                    Ok(None) => panic!("event stream ended before COMPLETED"),
                    Err(_) => panic!("timed out waiting for the next event"),
                }
            }
            assert!(saw_completed, "never observed the task reaching COMPLETED");

            shutdown.cancel();
            let _ = server.await;
            let _ = std::fs::remove_file(&sock);
        }
    }
}

#[cfg(not(unix))]
mod imp {
    use std::sync::Arc;

    use valyria_util::CancellationToken;

    use crate::runtime::Runtime;

    /// Non-Unix stub: the daemon speaks over a Unix-domain socket, which
    /// this platform does not provide. `valyria serve` therefore fails
    /// cleanly here; every embedded (in-process) command still works.
    pub async fn serve(
        _runtime: Arc<Runtime>,
        _socket_path: impl AsRef<std::path::Path>,
        _shutdown: CancellationToken,
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the valyria daemon requires a Unix platform (Unix-domain socket transport)",
        ))
    }
}

pub use imp::serve;
