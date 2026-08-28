//! The HTTP seam. The adapter drives an [`HttpTransport`]; it never opens a
//! socket itself. This keeps request-building, response-parsing, streaming
//! and cancellation testable offline against [`MockTransport`]. A real
//! `reqwest`/`hyper` implementation is a ~60-line impl of this trait and is
//! deliberately out of scope for the offline Phase 9 build.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::Value;

/// Transport-level failure, before any model semantics. Mapped to
/// [`valyria_model::ModelError`] by the adapter.
#[derive(Debug, Clone, thiserror::Error)]
pub enum HttpError {
    #[error("could not reach the model server: {0}")]
    Unreachable(String),
    #[error("model server returned status {status}: {body}")]
    Status { status: u16, body: String },
    #[error("malformed response from model server: {0}")]
    Malformed(String),
}

pub type HttpResult<T> = std::result::Result<T, HttpError>;

#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn get(&self, path: &str) -> HttpResult<Vec<u8>>;

    async fn post_json(&self, path: &str, body: Value) -> HttpResult<Vec<u8>>;

    /// POST `body` and stream back the Server-Sent-Events `data:` payloads,
    /// each already stripped of the `data: ` prefix and trailing newline.
    /// The sentinel `[DONE]` is passed through verbatim.
    fn post_sse(&self, path: &str, body: Value) -> BoxStream<'static, HttpResult<String>>;
}

/// Scripted transport for tests. Serves canned bodies per path, records the
/// last request body seen, and can be flipped to "server down".
pub struct MockTransport {
    responses: Mutex<HashMap<String, Vec<u8>>>,
    sse: Mutex<HashMap<String, Vec<String>>>,
    last_body: Mutex<Option<Value>>,
    down: Mutex<bool>,
}

impl MockTransport {
    pub fn new() -> Self {
        Self {
            responses: Mutex::new(HashMap::new()),
            sse: Mutex::new(HashMap::new()),
            last_body: Mutex::new(None),
            down: Mutex::new(false),
        }
    }

    pub fn with_response(self, path: &str, body: impl Into<Vec<u8>>) -> Self {
        self.responses
            .lock()
            .unwrap()
            .insert(path.to_string(), body.into());
        self
    }

    pub fn with_json_response(self, path: &str, body: Value) -> Self {
        let bytes = serde_json::to_vec(&body).unwrap();
        self.with_response(path, bytes)
    }

    /// Register the ordered `data:` payloads a `post_sse` to `path` yields.
    pub fn with_sse(self, path: &str, events: Vec<impl Into<String>>) -> Self {
        self.sse.lock().unwrap().insert(
            path.to_string(),
            events.into_iter().map(Into::into).collect(),
        );
        self
    }

    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap() = down;
    }

    pub fn last_body(&self) -> Option<Value> {
        self.last_body.lock().unwrap().clone()
    }
}

impl Default for MockTransport {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HttpTransport for MockTransport {
    async fn get(&self, path: &str) -> HttpResult<Vec<u8>> {
        if *self.down.lock().unwrap() {
            return Err(HttpError::Unreachable(format!("GET {path}")));
        }
        self.responses
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| HttpError::Status {
                status: 404,
                body: format!("no mock for GET {path}"),
            })
    }

    async fn post_json(&self, path: &str, body: Value) -> HttpResult<Vec<u8>> {
        *self.last_body.lock().unwrap() = Some(body);
        if *self.down.lock().unwrap() {
            return Err(HttpError::Unreachable(format!("POST {path}")));
        }
        self.responses
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .ok_or_else(|| HttpError::Status {
                status: 404,
                body: format!("no mock for POST {path}"),
            })
    }

    fn post_sse(&self, path: &str, body: Value) -> BoxStream<'static, HttpResult<String>> {
        *self.last_body.lock().unwrap() = Some(body);
        if *self.down.lock().unwrap() {
            let path = path.to_string();
            return stream::once(
                async move { Err(HttpError::Unreachable(format!("POST {path}"))) },
            )
            .boxed();
        }
        let events = self
            .sse
            .lock()
            .unwrap()
            .get(path)
            .cloned()
            .unwrap_or_default();
        stream::iter(events.into_iter().map(Ok)).boxed()
    }
}
