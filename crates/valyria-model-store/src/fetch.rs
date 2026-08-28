//! The download transport seam. `valyria-model-store` never speaks HTTP
//! directly — it drives a [`Fetcher`], so the resumable-download and
//! integrity logic is testable offline against [`InMemoryFetcher`] and a
//! real `reqwest`/`hyper` implementation is a small, isolated addition
//! (deliberately not in this crate's default build — Phase 9 scope note).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::error::{ModelStoreError, Result};

/// What a `HEAD` (or equivalent) tells us about a remote weights file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteObject {
    pub len: u64,
    pub etag: Option<String>,
    /// Whether the server honours `Range` requests. When `false` the
    /// downloader must start from zero every time (no resume).
    pub supports_ranges: bool,
}

#[async_trait]
pub trait Fetcher: Send + Sync {
    async fn head(&self, url: &str) -> Result<RemoteObject>;

    /// Bytes `[start, end)` of `url`. `end` is clamped to the object length
    /// by the implementation; a well-behaved caller never over-reads.
    async fn get_range(&self, url: &str, start: u64, end: u64) -> Result<Vec<u8>>;
}

/// In-memory fetcher for tests. Serves a fixed byte map, counts bytes
/// served, and can be told to fail once after N bytes to exercise the
/// resume path.
pub struct InMemoryFetcher {
    objects: HashMap<String, Vec<u8>>,
    etags: HashMap<String, String>,
    supports_ranges: bool,
    served: AtomicU64,
    /// `Some(n)` → the next `get_range` that would push cumulative served
    /// bytes past `n` fails with a transient error, then the latch clears.
    fail_after: Mutex<Option<u64>>,
}

impl InMemoryFetcher {
    pub fn new() -> Self {
        Self {
            objects: HashMap::new(),
            etags: HashMap::new(),
            supports_ranges: true,
            served: AtomicU64::new(0),
            fail_after: Mutex::new(None),
        }
    }

    pub fn with_object(mut self, url: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        let url = url.into();
        let bytes = bytes.into();
        let etag = format!("\"{}\"", valyria_util::ContentHash::of_bytes(&bytes));
        self.etags.insert(url.clone(), etag);
        self.objects.insert(url, bytes);
        self
    }

    pub fn without_range_support(mut self) -> Self {
        self.supports_ranges = false;
        self
    }

    /// After this many cumulative bytes served, the next range request
    /// fails once (transient). Used to test resume.
    pub fn fail_once_after(&self, n: u64) {
        *self.fail_after.lock().unwrap() = Some(n);
    }

    pub fn bytes_served(&self) -> u64 {
        self.served.load(Ordering::SeqCst)
    }
}

impl Default for InMemoryFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Fetcher for InMemoryFetcher {
    async fn head(&self, url: &str) -> Result<RemoteObject> {
        let obj = self
            .objects
            .get(url)
            .ok_or_else(|| ModelStoreError::Download {
                id: url.to_string(),
                detail: "no such object".into(),
            })?;
        Ok(RemoteObject {
            len: obj.len() as u64,
            etag: self.etags.get(url).cloned(),
            supports_ranges: self.supports_ranges,
        })
    }

    async fn get_range(&self, url: &str, start: u64, end: u64) -> Result<Vec<u8>> {
        let obj = self
            .objects
            .get(url)
            .ok_or_else(|| ModelStoreError::Download {
                id: url.to_string(),
                detail: "no such object".into(),
            })?;
        let len = obj.len() as u64;
        let start = start.min(len);
        let end = end.min(len).max(start);

        let prospective = self.served.load(Ordering::SeqCst) + (end - start);
        {
            let mut latch = self.fail_after.lock().unwrap();
            if let Some(threshold) = *latch {
                if prospective > threshold {
                    *latch = None; // one-shot
                    return Err(ModelStoreError::Download {
                        id: url.to_string(),
                        detail: "simulated transient network failure".into(),
                    });
                }
            }
        }

        self.served.fetch_add(end - start, Ordering::SeqCst);
        Ok(obj[start as usize..end as usize].to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn serves_ranges_and_counts_bytes() {
        let f = InMemoryFetcher::new().with_object("u", b"0123456789".to_vec());
        assert_eq!(f.head("u").await.unwrap().len, 10);
        assert_eq!(f.get_range("u", 0, 4).await.unwrap(), b"0123");
        assert_eq!(f.get_range("u", 4, 100).await.unwrap(), b"456789");
        assert_eq!(f.bytes_served(), 10);
    }

    #[tokio::test]
    async fn fail_once_after_is_one_shot() {
        let f = InMemoryFetcher::new().with_object("u", vec![7u8; 100]);
        f.fail_once_after(10);
        assert!(f.get_range("u", 0, 50).await.is_err());
        assert!(f.get_range("u", 0, 50).await.is_ok());
    }
}
