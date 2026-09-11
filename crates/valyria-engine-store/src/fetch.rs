//! The download transport seam, mirroring `valyria-model-store`'s
//! `Fetcher` (deliberately not shared — this crate owns its own error
//! type so a transport failure downloading an *engine* archive is never
//! mistaken for one downloading a *model*). Resumable-download logic is
//! testable offline against [`InMemoryFetcher`]; the real transport is
//! [`HttpFetcher`] behind the `http` feature.

use async_trait::async_trait;

use crate::error::{EngineStoreError, Result};

/// What a `HEAD` (or equivalent) tells us about a remote archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteObject {
    pub len: u64,
    pub supports_ranges: bool,
}

#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn head(&self, url: &str) -> Result<RemoteObject>;

    /// Bytes `[start, end)` of `url`. `end` is clamped to the object
    /// length by the implementation; a well-behaved caller never
    /// over-reads.
    async fn get_range(&self, url: &str, start: u64, end: u64) -> Result<Vec<u8>>;
}

/// In-memory fetcher for tests. Serves a fixed byte map and counts bytes
/// served.
#[derive(Default)]
pub struct InMemoryFetcher {
    objects: std::collections::HashMap<String, Vec<u8>>,
    supports_ranges: bool,
}

impl InMemoryFetcher {
    pub fn new() -> Self {
        Self {
            objects: std::collections::HashMap::new(),
            supports_ranges: true,
        }
    }

    pub fn with_object(mut self, url: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        self.objects.insert(url.into(), bytes.into());
        self
    }
}

#[async_trait]
impl Fetcher for InMemoryFetcher {
    async fn head(&self, url: &str) -> Result<RemoteObject> {
        let obj = self
            .objects
            .get(url)
            .ok_or_else(|| EngineStoreError::Download {
                component: "test".into(),
                version: "test".into(),
                detail: format!("no such object: {url}"),
            })?;
        Ok(RemoteObject {
            len: obj.len() as u64,
            supports_ranges: self.supports_ranges,
        })
    }

    async fn get_range(&self, url: &str, start: u64, end: u64) -> Result<Vec<u8>> {
        let obj = self
            .objects
            .get(url)
            .ok_or_else(|| EngineStoreError::Download {
                component: "test".into(),
                version: "test".into(),
                detail: format!("no such object: {url}"),
            })?;
        let len = obj.len() as u64;
        let start = start.min(len);
        let end = end.min(len).max(start);
        Ok(obj[start as usize..end as usize].to_vec())
    }
}

#[cfg(feature = "http")]
pub use http_fetcher::HttpFetcher;

#[cfg(feature = "http")]
mod http_fetcher {
    use async_trait::async_trait;
    use reqwest::header::{ACCEPT_RANGES, CONTENT_LENGTH, RANGE};

    use super::{Fetcher, RemoteObject};
    use crate::error::{EngineStoreError, Result};

    /// HTTPS archive downloader over a single pooled `reqwest` client with
    /// a `rustls` TLS backend (no OpenSSL / system TLS dependency).
    #[derive(Debug, Clone)]
    pub struct HttpFetcher {
        client: reqwest::Client,
    }

    impl HttpFetcher {
        pub fn new() -> Result<Self> {
            let client = reqwest::Client::builder()
                .use_rustls_tls()
                .https_only(true)
                .user_agent(concat!("valyria-engine-store/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| err(e.to_string()))?;
            Ok(Self { client })
        }
    }

    fn err(detail: String) -> EngineStoreError {
        EngineStoreError::Download {
            component: "engine".into(),
            version: "".into(),
            detail,
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
                .map_err(|e| err(e.to_string()))?
                .error_for_status()
                .map_err(|e| err(e.to_string()))?;
            let headers = resp.headers();
            let len = headers
                .get(CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(|| err("HEAD response had no usable Content-Length".into()))?;
            let supports_ranges = headers
                .get(ACCEPT_RANGES)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.eq_ignore_ascii_case("bytes"))
                .unwrap_or(false);
            Ok(RemoteObject {
                len,
                supports_ranges,
            })
        }

        async fn get_range(&self, url: &str, start: u64, end: u64) -> Result<Vec<u8>> {
            let last = end.saturating_sub(1).max(start);
            let resp = self
                .client
                .get(url)
                .header(RANGE, format!("bytes={start}-{last}"))
                .send()
                .await
                .map_err(|e| err(e.to_string()))?
                .error_for_status()
                .map_err(|e| err(e.to_string()))?;
            let bytes = resp.bytes().await.map_err(|e| err(e.to_string()))?;
            Ok(bytes.to_vec())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serves_ranges() {
        let f = InMemoryFetcher::new().with_object("u", b"0123456789".to_vec());
        assert_eq!(f.head("u").await.unwrap().len, 10);
        assert_eq!(f.get_range("u", 0, 4).await.unwrap(), b"0123");
        assert_eq!(f.get_range("u", 4, 100).await.unwrap(), b"456789");
    }
}
