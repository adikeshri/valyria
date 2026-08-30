//! Newline-delimited JSON framing over a Unix domain socket, and
//! [`SocketClient`] — the daemon-transport implementation of
//! [`Client`](crate::Client) (§4.27, D11).
//!
//! The framing is deliberately the simplest thing that works: one JSON
//! object per line. A [`ClientFrame`] goes up, a [`ServerFrame`] (or a
//! stream of them, for a subscription) comes back. The daemon's
//! accept-loop lives in `valyria_app::daemon`; it dispatches each
//! `ClientFrame` straight into an `EmbeddedClient`, so the socket path and
//! the in-process path run *identical* runtime code.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::stream::{BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
#[cfg(any(unix, windows))]
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::UnixStream;
use tokio::sync::Mutex;

use crate::client::Client;
use crate::envelope::{Request, Response};
use crate::messages::{WireError, WireEvent};

/// A frame sent by a client to the daemon.
///
/// Externally tagged (serde's default) *on purpose*: `ServerFrame::Event`
/// wraps a `WireEvent` whose `payload` is a `serde_json::Value`, and
/// `serde_json::Value` does not survive serde's internally/adjacently
/// tagged content buffer. External tagging deserializes each variant's
/// body with the real deserializer, so the payload round-trips exactly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClientFrame {
    /// A one-shot request expecting exactly one [`ServerFrame::Response`].
    Call(Request),
    /// Open an event stream from `since`; the connection then carries
    /// [`ServerFrame::Event`] frames until the client hangs up.
    ///
    /// `task_id`, when set (protocol 1.7.0, G11), restricts the stream to
    /// that task's events plus workspace-global (task-less) events. Old
    /// frames omit it and get the full stream, unchanged.
    Subscribe {
        since: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
    /// [`Self::Call`] with a per-daemon auth token (protocol 1.6.0, G10).
    /// A daemon started with a token **requires** this variant and rejects
    /// bare [`Self::Call`] with `auth.required`; a daemon without one
    /// accepts either.
    AuthCall { token: String, request: Request },
    /// [`Self::Subscribe`] with the auth token — see [`Self::AuthCall`].
    AuthSubscribe {
        token: String,
        since: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        task_id: Option<String>,
    },
}

/// A frame sent by the daemon to a client. Externally tagged — see
/// [`ClientFrame`] for why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerFrame {
    Response(Response),
    Event(WireEvent),
}

/// Serialize a frame as one line (no embedded newlines — `serde_json`
/// compact output never emits them).
pub fn encode_line<T: Serialize>(frame: &T) -> String {
    let mut s = serde_json::to_string(frame).expect("wire frame serializes");
    s.push('\n');
    s
}

/// The daemon-transport `Client`: every call opens a short-lived
/// connection, writes one [`ClientFrame::Call`], reads one
/// [`ServerFrame::Response`]. `subscribe_events` holds its connection open
/// for the life of the returned stream.
///
/// A connect/IO failure is surfaced as a `Response::Error` with code
/// `protocol.transport` rather than a panic — a CLI talking to a daemon
/// that just went away should print a clean error, not unwind.
pub struct SocketClient {
    path: PathBuf,
    /// Per-daemon auth token (G10). When set, frames go out as
    /// [`ClientFrame::AuthCall`] / [`ClientFrame::AuthSubscribe`]. Only the
    /// unix transport reads it; the non-unix stub is a hard error.
    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    token: Option<String>,
    /// Serializes concurrent `call`s that would otherwise race to open
    /// their own connections — harmless, but this keeps the socket's
    /// accept rate sane and makes tests deterministic.
    connect_lock: Mutex<()>,
}

impl SocketClient {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            token: None,
            connect_lock: Mutex::new(()),
        }
    }

    /// A client that authenticates every frame with `token` (G10).
    pub fn with_token(path: impl Into<PathBuf>, token: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            token: Some(token.into()),
            connect_lock: Mutex::new(()),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    fn call_frame(&self, request: Request) -> ClientFrame {
        match &self.token {
            Some(token) => ClientFrame::AuthCall {
                token: token.clone(),
                request,
            },
            None => ClientFrame::Call(request),
        }
    }

    #[cfg_attr(not(any(unix, windows)), allow(dead_code))]
    fn subscribe_frame(&self, since: u64, task_id: Option<String>) -> ClientFrame {
        match &self.token {
            Some(token) => ClientFrame::AuthSubscribe {
                token: token.clone(),
                since,
                task_id,
            },
            None => ClientFrame::Subscribe { since, task_id },
        }
    }

    /// Open a fresh transport connection to the daemon. On Unix this is a
    /// Unix-domain socket; on Windows a named pipe (G9). Both platforms
    /// yield a stream that satisfies the generic framing helpers below.
    #[cfg(unix)]
    async fn connect(&self) -> std::io::Result<UnixStream> {
        UnixStream::connect(&self.path).await
    }

    #[cfg(windows)]
    async fn connect(&self) -> std::io::Result<tokio::net::windows::named_pipe::NamedPipeClient> {
        use tokio::net::windows::named_pipe::ClientOptions;
        // ERROR_PIPE_BUSY (231): the server is between accept instances.
        const ERROR_PIPE_BUSY: i32 = 231;
        for _ in 0..100 {
            match ClientOptions::new().open(self.path.as_os_str()) {
                Ok(client) => return Ok(client),
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "named pipe stayed busy",
        ))
    }
}

/// Write one frame and read exactly one `Response` back.
#[cfg(any(unix, windows))]
async fn run_call<S>(stream: S, frame: ClientFrame) -> std::io::Result<Response>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    write_half.write_all(encode_line(&frame).as_bytes()).await?;
    write_half.flush().await?;
    let mut lines = BufReader::new(read_half).lines();
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "daemon closed"))?;
    match serde_json::from_str::<ServerFrame>(&line) {
        Ok(ServerFrame::Response(resp)) => Ok(resp),
        Ok(ServerFrame::Event(_)) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "daemon sent an event in response to a call",
        )),
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
    }
}

/// Write one subscribe frame and stream `Event` frames until the
/// connection ends. The write half is kept alive for the stream's life.
#[cfg(any(unix, windows))]
async fn run_subscribe<S>(stream: S, frame: ClientFrame) -> BoxStream<'static, WireEvent>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    if write_half
        .write_all(encode_line(&frame).as_bytes())
        .await
        .is_err()
    {
        return futures::stream::empty().boxed();
    }
    let _ = write_half.flush().await;
    let lines = BufReader::new(read_half).lines();
    futures::stream::unfold((lines, write_half), |(mut lines, write_half)| async move {
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => match serde_json::from_str::<ServerFrame>(&line) {
                    Ok(ServerFrame::Event(ev)) => return Some((ev, (lines, write_half))),
                    _ => continue,
                },
                _ => return None,
            }
        }
    })
    .boxed()
}

fn transport_error(message: impl std::fmt::Display) -> Response {
    Response::Error(WireError {
        code: "protocol.transport".to_string(),
        message: message.to_string(),
        retryable: true,
    })
}

#[cfg(not(any(unix, windows)))]
#[async_trait::async_trait]
impl Client for SocketClient {
    async fn call(&self, _req: Request) -> Response {
        transport_error("the valyria daemon transport requires a Unix or Windows platform")
    }

    async fn subscribe_events(&self, _since: u64) -> BoxStream<'static, WireEvent> {
        futures::stream::empty().boxed()
    }

    async fn subscribe_events_for_task(
        &self,
        _since: u64,
        _task_id: Option<String>,
    ) -> BoxStream<'static, WireEvent> {
        futures::stream::empty().boxed()
    }
}

#[cfg(any(unix, windows))]
#[async_trait::async_trait]
impl Client for SocketClient {
    async fn call(&self, req: Request) -> Response {
        let _guard = self.connect_lock.lock().await;
        let stream = match self.connect().await {
            Ok(s) => s,
            Err(e) => return transport_error(e),
        };
        match run_call(stream, self.call_frame(req)).await {
            Ok(resp) => resp,
            Err(e) => transport_error(e),
        }
    }

    async fn subscribe_events(&self, since: u64) -> BoxStream<'static, WireEvent> {
        self.subscribe_inner(since, None).await
    }

    async fn subscribe_events_for_task(
        &self,
        since: u64,
        task_id: Option<String>,
    ) -> BoxStream<'static, WireEvent> {
        self.subscribe_inner(since, task_id).await
    }
}

#[cfg(any(unix, windows))]
impl SocketClient {
    async fn subscribe_inner(
        &self,
        since: u64,
        task_id: Option<String>,
    ) -> BoxStream<'static, WireEvent> {
        let stream = match self.connect().await {
            Ok(s) => s,
            Err(_) => return futures::stream::empty().boxed(),
        };
        run_subscribe(stream, self.subscribe_frame(since, task_id)).await
    }
}

/// A `SocketClient` behind an `Arc<dyn Client>`, for callers that want the
/// trait object directly.
pub fn connect(path: impl Into<PathBuf>) -> Arc<dyn Client> {
    Arc::new(SocketClient::new(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{HelloRequest, HelloResponse};

    #[test]
    fn client_frame_round_trips() {
        let f = ClientFrame::Call(Request::Hello(HelloRequest {
            client_name: "cli".into(),
        }));
        let line = encode_line(&f);
        assert!(line.ends_with('\n'));
        assert!(!line.trim_end().contains('\n'));
        let back: ClientFrame = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn server_frame_round_trips() {
        let f = ServerFrame::Response(Response::Hello(HelloResponse {
            protocol_version: "1.0.0".into(),
            runtime_version: "0.1.0".into(),
            capabilities: vec!["doctor".into()],
        }));
        let line = encode_line(&f);
        let back: ServerFrame = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(f, back);
    }

    #[tokio::test]
    async fn call_against_a_missing_socket_is_a_clean_transport_error() {
        let client = SocketClient::new("/nonexistent/valyria.sock");
        let resp = client
            .call(Request::Hello(HelloRequest {
                client_name: "cli".into(),
            }))
            .await;
        match resp {
            Response::Error(e) => assert_eq!(e.code, "protocol.transport"),
            other => panic!("expected transport error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscribe_against_a_missing_socket_is_an_empty_stream() {
        let client = SocketClient::new("/nonexistent/valyria.sock");
        let mut stream = client.subscribe_events(0).await;
        assert!(stream.next().await.is_none());
    }
}
