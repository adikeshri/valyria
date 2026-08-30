//! `reqwest` + `rustls` implementation of [`Fetcher`] (§4.21).
//!
//! Ranged `GET` for resumable downloads plus a `HEAD` for the object's
//! length / etag / range support. Retry, backoff and `.part` resume are
//! the store's chunk loop's job, not this transport's — this stays a thin
//! HTTP shim so the interesting logic remains offline-testable against
//! `InMemoryFetcher`.
//!
//! Compiled only with the default `http` feature.

use async_trait::async_trait;
use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, ETAG, RANGE};

use crate::error::{ModelStoreError, Result};
use crate::fetch::{Fetcher, RemoteObject};

/// HTTPS weights downloader over a single pooled `reqwest` client with a
/// `rustls` TLS backend (no OpenSSL / system TLS dependency).
#[derive(Debug, Clone)]
pub struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .https_only(true)
            .user_agent(concat!("valyria-model-store/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| err("<client>", e))?;
        Ok(Self { client })
    }
}

fn err(url: &str, e: impl std::fmt::Display) -> ModelStoreError {
    ModelStoreError::Download {
        id: url.to_string(),
        detail: e.to_string(),
    }
}

#[async_trait]
impl Fetcher for HttpFetcher {
    async fn head(&self, url: &str) -> Result<RemoteObject> {
        let resp = self
            .client
            .head(url)
            .send()
            .await
            .map_err(|e| err(url, e))?
            .error_for_status()
            .map_err(|e| err(url, e))?;
        let headers = resp.headers();
        let len = headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| err(url, "HEAD response had no usable Content-Length"))?;
        let etag = headers
            .get(ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        let supports_ranges = headers
            .get(ACCEPT_RANGES)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.eq_ignore_ascii_case("bytes"))
            .unwrap_or(false);
        Ok(RemoteObject {
            len,
            etag,
            supports_ranges,
        })
    }

    async fn get_range(&self, url: &str, start: u64, end: u64) -> Result<Vec<u8>> {
        // The store passes a half-open `[start, end)`; HTTP `Range` is
        // inclusive on both ends.
        let last = end.saturating_sub(1).max(start);
        let resp = self
            .client
            .get(url)
            .header(RANGE, format!("bytes={start}-{last}"))
            .send()
            .await
            .map_err(|e| err(url, e))?
            .error_for_status()
            .map_err(|e| err(url, e))?;
        let bytes = resp.bytes().await.map_err(|e| err(url, e))?;
        Ok(bytes.to_vec())
    }
}
