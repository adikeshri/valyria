//! The daemon accept-loop (§4.27, D3-alt: "Daemon vs embedded — default:
//! both"). `serve` listens on a local IPC endpoint and dispatches every
//! framed [`ClientFrame`] straight into an [`EmbeddedClient`] — so a CLI
//! talking to the daemon over the socket and a CLI running the runtime
//! in-process execute *identical* code past this point. That identity is
//! the whole point of D11.
//!
//! Transport is a Unix-domain socket on Unix and a named pipe on Windows
//! (G9). The frame handling ([`framed::serve_connection`]) is shared; only
//! the listener and the peer check differ. On a platform with neither,
//! [`serve`] returns an `Unsupported` error and the embedded (in-process)
//! path is unaffected.

// --- transport-agnostic frame handling -------------------------------------

#[cfg(any(unix, windows))]
mod framed {
    use std::sync::Arc;

    use futures::StreamExt;
    use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
    use valyria_protocol::transport::{encode_line, ClientFrame, ServerFrame};
    use valyria_protocol::Client;

    use crate::client::EmbeddedClient;

    pub(super) fn reject(code: &str, message: impl Into<String>) -> ServerFrame {
        ServerFrame::Response(valyria_protocol::Response::Error(
            valyria_protocol::WireError {
                code: code.into(),
                message: message.into(),
                retryable: false,
            },
        ))
    }

    enum Resolved {
        Call(valyria_protocol::Request),
        Subscribe { since: u64, task_id: Option<String> },
    }

    /// Handle one already-connected, already-peer-checked stream: read one
    /// [`ClientFrame`], authenticate it against `auth_token`, and either
    /// answer a call or stream a subscription until the client hangs up.
    pub(super) async fn serve_connection<S>(
        stream: S,
        client: Arc<EmbeddedClient>,
        auth_token: Arc<Option<String>>,
    ) -> std::io::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut lines = BufReader::new(read_half).lines();

        let Some(first) = lines.next_line().await? else {
            return Ok(());
        };
        let frame: ClientFrame = match serde_json::from_str(&first) {
            Ok(f) => f,
            Err(e) => {
                write_half
                    .write_all(encode_line(&reject("protocol.bad_frame", e.to_string())).as_bytes())
                    .await?;
                return Ok(());
            }
        };

        let required = auth_token.as_ref().as_deref();
        let resolved: Result<Resolved, ServerFrame> = match frame {
            ClientFrame::AuthCall { token, request } => match required {
                Some(want) if want == token => Ok(Resolved::Call(request)),
                Some(_) => Err(reject("auth.token_mismatch", "invalid auth token")),
                None => Ok(Resolved::Call(request)),
            },
            ClientFrame::AuthSubscribe {
                token,
                since,
                task_id,
            } => match required {
                Some(want) if want == token => Ok(Resolved::Subscribe { since, task_id }),
                Some(_) => Err(reject("auth.token_mismatch", "invalid auth token")),
                None => Ok(Resolved::Subscribe { since, task_id }),
            },
            ClientFrame::Call(request) => match required {
                None => Ok(Resolved::Call(request)),
                Some(_) => Err(reject(
                    "auth.required",
                    "this daemon requires an auth token; use AuthCall",
                )),
            },
            ClientFrame::Subscribe { since, task_id } => match required {
                None => Ok(Resolved::Subscribe { since, task_id }),
                Some(_) => Err(reject(
                    "auth.required",
                    "this daemon requires an auth token; use AuthSubscribe",
                )),
            },
        };

        match resolved {
            Err(frame) => {
                write_half.write_all(encode_line(&frame).as_bytes()).await?;
            }
            Ok(Resolved::Call(req)) => {
                let resp = client.call(req).await;
                write_half
                    .write_all(encode_line(&ServerFrame::Response(resp)).as_bytes())
                    .await?;
                write_half.flush().await?;
            }
            Ok(Resolved::Subscribe { since, task_id }) => {
                let mut events = client.subscribe_events_for_task(since, task_id).await;
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
}

// --- Unix: Unix-domain socket --------------------------------------------

#[cfg(unix)]
mod imp {
    use std::sync::Arc;

    use tokio::io::AsyncWriteExt;
    use tokio::net::UnixListener;
    use valyria_protocol::transport::encode_line;
    use valyria_util::CancellationToken;

    use super::framed::{reject, serve_connection};
    use crate::client::EmbeddedClient;
    use crate::runtime::Runtime;

    /// Bind `socket_path` and serve until `shutdown` is triggered.
    ///
    /// `auth_token`, when `Some`, is the per-daemon token every client
    /// frame must present (G10). Independent of it, every connection is
    /// checked to come from the **same OS user** as the daemon
    /// (`SO_PEERCRED` / `getpeereid`); a foreign uid is rejected with
    /// `auth.peer_uid`.
    pub async fn serve(
        runtime: Arc<Runtime>,
        socket_path: impl AsRef<std::path::Path>,
        shutdown: CancellationToken,
        auth_token: Option<String>,
    ) -> std::io::Result<()> {
        let socket_path = socket_path.as_ref().to_path_buf();
        let _ = std::fs::remove_file(&socket_path);
        let listener = UnixListener::bind(&socket_path)?;
        tracing::info!(
            path = %socket_path.display(),
            authenticated = auth_token.is_some(),
            "valyria daemon listening (unix socket)"
        );

        let client = Arc::new(EmbeddedClient::new(runtime));
        let auth_token = Arc::new(auth_token);
        let own_uid = rustix::process::geteuid().as_raw();

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                accepted = listener.accept() => {
                    match accepted {
                        Ok((stream, _addr)) => {
                            let client = client.clone();
                            let auth_token = auth_token.clone();
                            tokio::spawn(async move {
                                if let Err(e) = handle(stream, client, auth_token, own_uid).await {
                                    tracing::debug!(%e, "daemon connection ended");
                                }
                            });
                        }
                        Err(e) => tracing::warn!(%e, "daemon accept failed"),
                    }
                }
            }
        }

        let _ = std::fs::remove_file(&socket_path);
        Ok(())
    }

    async fn handle(
        stream: tokio::net::UnixStream,
        client: Arc<EmbeddedClient>,
        auth_token: Arc<Option<String>>,
        own_uid: u32,
    ) -> std::io::Result<()> {
        // Peer-uid gate: only the daemon's own OS user may connect.
        let peer = stream.peer_cred();
        match peer {
            Ok(cred) if cred.uid() == own_uid => serve_connection(stream, client, auth_token).await,
            Ok(cred) => {
                deny(
                    stream,
                    format!("connection from uid {} refused", cred.uid()),
                )
                .await
            }
            Err(e) => deny(stream, format!("peer credentials: {e}")).await,
        }
    }

    async fn deny(stream: tokio::net::UnixStream, message: String) -> std::io::Result<()> {
        let (_r, mut w) = stream.into_split();
        w.write_all(encode_line(&reject("auth.peer_uid", message)).as_bytes())
            .await?;
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use valyria_protocol::transport::SocketClient;
        use valyria_protocol::{Client, HelloRequest, Request, Response};

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
            let server = tokio::spawn(serve(rt.clone(), sock.clone(), shutdown.clone(), None));
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
            let server = tokio::spawn(serve(rt.clone(), sock.clone(), shutdown.clone(), None));
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
            let server = tokio::spawn(serve(rt.clone(), sock.clone(), shutdown.clone(), None));
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
                match tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    futures::StreamExt::next(&mut events),
                )
                .await
                {
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

// --- Windows: named pipe (G9) ------------------------------------------------

#[cfg(windows)]
mod imp {
    use std::sync::Arc;

    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    use valyria_util::CancellationToken;

    use super::framed::serve_connection;
    use crate::client::EmbeddedClient;
    use crate::runtime::Runtime;

    /// Serve on a Windows named pipe until `shutdown`. `pipe_path` is a
    /// pipe name (`\\.\pipe\valyria-<id>`); a plain filesystem path is
    /// mapped to `\\.\pipe\<file-name>` for convenience so callers can
    /// pass the same "socket path" they would on Unix.
    ///
    /// Named pipes created without an explicit `SECURITY_ATTRIBUTES` are
    /// reachable only by the creating user's token, which is the peer
    /// boundary here (the Unix `SO_PEERCRED` analogue); `auth_token` adds
    /// the same per-frame token check as the Unix path (G10).
    pub async fn serve(
        runtime: Arc<Runtime>,
        pipe_path: impl AsRef<std::path::Path>,
        shutdown: CancellationToken,
        auth_token: Option<String>,
    ) -> std::io::Result<()> {
        let name = pipe_name(pipe_path.as_ref());
        tracing::info!(
            pipe = %name,
            authenticated = auth_token.is_some(),
            "valyria daemon listening (named pipe)"
        );

        let client = Arc::new(EmbeddedClient::new(runtime));
        let auth_token = Arc::new(auth_token);

        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)?;

        loop {
            tokio::select! {
                _ = shutdown.cancelled() => break,
                connected = server.connect() => {
                    connected?;
                    // Hand off the connected instance and immediately open
                    // the next one so there is no accept gap.
                    let this = std::mem::replace(&mut server, ServerOptions::new().create(&name)?);
                    let client = client.clone();
                    let auth_token = auth_token.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle(this, client, auth_token).await {
                            tracing::debug!(%e, "daemon connection ended");
                        }
                    });
                }
            }
        }
        Ok(())
    }

    async fn handle(
        stream: NamedPipeServer,
        client: Arc<EmbeddedClient>,
        auth_token: Arc<Option<String>>,
    ) -> std::io::Result<()> {
        serve_connection(stream, client, auth_token).await
    }

    fn pipe_name(path: &std::path::Path) -> String {
        let s = path.to_string_lossy();
        if s.starts_with(r"\\.\pipe\") || s.starts_with(r"\\?\pipe\") {
            s.into_owned()
        } else {
            let leaf = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_else(|| "valyria".to_string());
            format!(r"\\.\pipe\{leaf}")
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use valyria_protocol::transport::SocketClient;
        use valyria_protocol::{Client, HelloRequest, Request, Response};

        #[tokio::test]
        async fn hello_over_the_named_pipe_matches_the_embedded_path() {
            let dir = tempfile::tempdir().unwrap();
            let cfg = crate::runtime::RuntimeConfig::new(dir.path())
                .with_global_dir(dir.path().join("global"));
            let rt = Arc::new(Runtime::open(cfg).await.unwrap());
            std::mem::forget(dir);

            let name = format!(r"\\.\pipe\valyria-test-{}", std::process::id());
            let shutdown = CancellationToken::new();
            let server = tokio::spawn(serve(
                rt.clone(),
                std::path::PathBuf::from(&name),
                shutdown.clone(),
                Some("tok".to_string()),
            ));
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;

            // Wrong token is refused; right token gets Hello.
            match SocketClient::new(&name)
                .call(Request::Hello(HelloRequest {
                    client_name: "t".into(),
                }))
                .await
            {
                Response::Error(e) => assert_eq!(e.code, "auth.required"),
                other => panic!("expected auth.required, got {other:?}"),
            }
            match SocketClient::with_token(&name, "tok")
                .call(Request::Hello(HelloRequest {
                    client_name: "t".into(),
                }))
                .await
            {
                Response::Hello(h) => {
                    assert_eq!(h.protocol_version, valyria_protocol::PROTOCOL_VERSION);
                    assert!(h.capabilities.iter().any(|c| c == "windows"));
                }
                other => panic!("expected Hello, got {other:?}"),
            }

            shutdown.cancel();
            let _ = server.await;
        }
    }
}

// --- other platforms ------------------------------------------------------

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::sync::Arc;

    use valyria_util::CancellationToken;

    use crate::runtime::Runtime;

    /// Stub for a platform with neither a Unix-domain socket nor a Windows
    /// named pipe: `valyria serve` fails cleanly; every embedded (in-process)
    /// command still works. The signature matches the unix/windows impls
    /// (incl. the G10 `auth_token`) so the single CLI call site compiles
    /// everywhere.
    pub async fn serve(
        _runtime: Arc<Runtime>,
        _socket_path: impl AsRef<std::path::Path>,
        _shutdown: CancellationToken,
        _auth_token: Option<String>,
    ) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the valyria daemon needs a Unix-domain socket or a Windows named pipe",
        ))
    }
}

pub use imp::serve;
