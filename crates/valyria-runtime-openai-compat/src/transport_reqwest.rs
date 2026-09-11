//! The real `reqwest` + `rustls` implementation of [`HttpTransport`].
//!
//! Compiled only with the default `http` feature. Everything interesting —
//! request construction, response parsing, SSE framing, cancellation —
//! lives in [`crate::wire`] and [`crate::runtime`] and is exercised
//! offline against [`crate::transport::MockTransport`]; this file is the
//! thin socket layer: a pooled client, a base URL, and an SSE line
//! splitter.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, BoxStream, StreamExt};
use serde_json::Value;

use crate::transport::{HttpError, HttpResult, HttpTransport};

/// A [`HttpTransport`] that talks to a local OpenAI-compatible server over
/// HTTP/1.1 with a `rustls` TLS backend (no OpenSSL / system TLS). The
/// base URL is an origin such as `http://127.0.0.1:8080`; every call joins
/// it with an absolute path (`/v1/chat/completions`, `/health`).
#[derive(Debug, Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
    base: String,
}

impl ReqwestTransport {
    /// `base` is a scheme + host + port with no trailing slash, e.g.
    /// `http://127.0.0.1:8080`. A trailing slash is trimmed so that
    /// `join("/health")` is always well formed.
    pub fn new(base: impl Into<String>) -> HttpResult<Self> {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .user_agent(concat!(
                "valyria-runtime-openai-compat/",
                env!("CARGO_PKG_VERSION")
            ))
            // Local inference can take a long time on the first token of a
            // cold model; do not impose a whole-request deadline here. The
            // caller cancels via the `CancellationToken` instead.
            .connect_timeout(Duration::from_secs(10))
            .pool_idle_timeout(Duration::from_secs(90))
            .build()
            .map_err(|e| HttpError::Unreachable(e.to_string()))?;
        Ok(Self {
            client,
            base: base.into().trim_end_matches('/').to_string(),
        })
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with('/') {
            format!("{}{}", self.base, path)
        } else {
            format!("{}/{}", self.base, path)
        }
    }
}

fn conn_err(e: reqwest::Error) -> HttpError {
    HttpError::Unreachable(e.to_string())
}

async fn read_body_or_status(resp: reqwest::Response) -> HttpResult<Vec<u8>> {
    let status = resp.status();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| HttpError::Malformed(e.to_string()))?;
    if status.is_success() {
        Ok(bytes.to_vec())
    } else {
        Err(HttpError::Status {
            status: status.as_u16(),
            body: String::from_utf8_lossy(&bytes).into_owned(),
        })
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn get(&self, path: &str) -> HttpResult<Vec<u8>> {
        let resp = self
            .client
            .get(self.url(path))
            .send()
            .await
            .map_err(conn_err)?;
        read_body_or_status(resp).await
    }

    async fn post_json(&self, path: &str, body: Value) -> HttpResult<Vec<u8>> {
        let resp = self
            .client
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .map_err(conn_err)?;
        read_body_or_status(resp).await
    }

    fn post_sse(&self, path: &str, body: Value) -> BoxStream<'static, HttpResult<String>> {
        let client = self.client.clone();
        let url = self.url(path);

        stream::once(async move {
            client
                .post(url)
                .header("accept", "text/event-stream")
                .json(&body)
                .send()
                .await
                .map_err(conn_err)
        })
        .flat_map(|res| match res {
            Err(e) => stream::once(async move { Err(e) }).boxed(),
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() {
                    return stream::once(async move {
                        let body = resp.text().await.unwrap_or_default();
                        Err(HttpError::Status {
                            status: status.as_u16(),
                            body,
                        })
                    })
                    .boxed();
                }
                let raw = resp.bytes_stream().map(|r| r.map(|b| b.to_vec())).boxed();
                sse_lines(raw).boxed()
            }
        })
        .boxed()
    }
}

/// Split a raw byte stream of `text/event-stream` into the payload of each
/// `data:` line — prefix and one optional leading space stripped, trailing
/// `\r` removed. `event:`, comment (`:`), and blank separator lines are
/// dropped. The `[DONE]` sentinel is passed through verbatim by virtue of
/// being an ordinary `data:` payload.
fn sse_lines(
    bytes: BoxStream<'static, reqwest::Result<Vec<u8>>>,
) -> BoxStream<'static, HttpResult<String>> {
    struct State {
        bytes: BoxStream<'static, reqwest::Result<Vec<u8>>>,
        buf: String,
        ready: VecDeque<String>,
        ended: bool,
    }

    let state = State {
        bytes,
        buf: String::new(),
        ready: VecDeque::new(),
        ended: false,
    };

    stream::unfold(state, |mut st| async move {
        loop {
            if let Some(line) = st.ready.pop_front() {
                return Some((Ok(line), st));
            }
            if st.ended {
                return None;
            }
            match st.bytes.next().await {
                None => {
                    st.ended = true;
                    // Flush any trailing unterminated `data:` line.
                    if let Some(payload) = take_data_line(st.buf.trim_end()) {
                        return Some((Ok(payload), st));
                    }
                    return None;
                }
                Some(Err(e)) => {
                    st.ended = true;
                    return Some((Err(HttpError::Malformed(e.to_string())), st));
                }
                Some(Ok(chunk)) => {
                    st.buf.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(nl) = st.buf.find('\n') {
                        let line: String = st.buf.drain(..=nl).collect();
                        if let Some(payload) = take_data_line(line.trim_end_matches(['\r', '\n'])) {
                            st.ready.push_back(payload);
                        }
                    }
                }
            }
        }
    })
    .boxed()
}

fn take_data_line(line: &str) -> Option<String> {
    let rest = line.strip_prefix("data:")?;
    Some(rest.strip_prefix(' ').unwrap_or(rest).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_join_handles_slashes() {
        let t = ReqwestTransport::new("http://127.0.0.1:8080/").unwrap();
        assert_eq!(t.url("/health"), "http://127.0.0.1:8080/health");
        assert_eq!(t.url("v1/x"), "http://127.0.0.1:8080/v1/x");
    }

    fn ok_chunk(b: &[u8]) -> reqwest::Result<Vec<u8>> {
        Ok(b.to_vec())
    }

    #[tokio::test]
    async fn sse_splitter_extracts_data_payloads() {
        let raw = "event: message\ndata: {\"a\":1}\n\n: keep-alive\ndata: [DONE]\n\n";
        let byte_stream = stream::iter(vec![ok_chunk(raw.as_bytes())]).boxed();
        let got: Vec<String> = sse_lines(byte_stream).map(|r| r.unwrap()).collect().await;
        assert_eq!(got, vec!["{\"a\":1}".to_string(), "[DONE]".to_string()]);
    }

    #[tokio::test]
    async fn sse_splitter_reassembles_across_chunk_boundaries() {
        let parts = vec![
            ok_chunk(b"data: {\"hel"),
            ok_chunk(b"lo\":true}\n\ndata: [DO"),
            ok_chunk(b"NE]\n\n"),
        ];
        let byte_stream = stream::iter(parts).boxed();
        let got: Vec<String> = sse_lines(byte_stream).map(|r| r.unwrap()).collect().await;
        assert_eq!(
            got,
            vec!["{\"hello\":true}".to_string(), "[DONE]".to_string()]
        );
    }
}
