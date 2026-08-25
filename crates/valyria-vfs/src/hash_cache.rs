//! Content hash cache, keyed by `(inode, mtime, size)` as specified in
//! §4.4 — recomputing a `blake3` hash is cheap for small files but adds up
//! across a large repo's index/context pipeline, so a file whose stat
//! hasn't changed since the last hash is never re-read.

use std::collections::HashMap;
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parking_lot::RwLock;
use valyria_util::ContentHash;

use crate::error::{Result, VfsError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StatKey {
    modified: SystemTime,
    size: u64,
    platform_id: Option<(u64, u64)>, // (dev, ino) on unix
}

impl StatKey {
    fn from_metadata(meta: &Metadata) -> std::io::Result<Self> {
        Ok(Self {
            modified: meta.modified()?,
            size: meta.len(),
            platform_id: platform_id(meta),
        })
    }
}

#[cfg(unix)]
fn platform_id(meta: &Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn platform_id(_meta: &Metadata) -> Option<(u64, u64)> {
    None
}

#[derive(Default)]
pub struct HashCache {
    entries: RwLock<HashMap<PathBuf, (StatKey, ContentHash)>>,
}

impl HashCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hash `path`'s current contents, reusing the cached hash if the
    /// file's stat is unchanged since it was last computed.
    pub fn hash_file(&self, path: &Path) -> Result<ContentHash> {
        let meta = std::fs::metadata(path).map_err(|e| io_err(path, e))?;
        let key = StatKey::from_metadata(&meta).map_err(|e| io_err(path, e))?;

        if let Some((cached_key, hash)) = self.entries.read().get(path) {
            if *cached_key == key {
                return Ok(*hash);
            }
        }

        let bytes = std::fs::read(path).map_err(|e| io_err(path, e))?;
        let hash = ContentHash::of_bytes(&bytes);
        self.entries.write().insert(path.to_path_buf(), (key, hash));
        Ok(hash)
    }

    pub fn invalidate(&self, path: &Path) {
        self.entries.write().remove(path);
    }

    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn io_err(path: &Path, source: std::io::Error) -> VfsError {
    VfsError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;
    use std::time::Duration;

    #[test]
    fn hashes_a_file_and_caches_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"content").unwrap();

        let cache = HashCache::new();
        let h1 = cache.hash_file(&path).unwrap();
        assert_eq!(cache.len(), 1);
        let h2 = cache.hash_file(&path).unwrap();
        assert_eq!(h1, h2);
    }

    #[test]
    fn detects_content_change_via_stat() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"version one").unwrap();

        let cache = HashCache::new();
        let h1 = cache.hash_file(&path).unwrap();

        // Ensure mtime actually advances on filesystems with coarse
        // resolution (some have 1s granularity).
        sleep(Duration::from_millis(20));
        std::fs::write(&path, b"version two, quite different content").unwrap();

        let h2 = cache.hash_file(&path).unwrap();
        assert_ne!(h1, h2);
    }

    #[test]
    fn invalidate_forces_a_rehash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"content").unwrap();

        let cache = HashCache::new();
        cache.hash_file(&path).unwrap();
        assert_eq!(cache.len(), 1);
        cache.invalidate(&path);
        assert!(cache.is_empty());
    }

    #[test]
    fn missing_file_is_an_io_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("never-existed.txt");
        let cache = HashCache::new();
        assert!(cache.hash_file(&path).is_err());
    }
}
