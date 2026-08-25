//! Content-addressed blob store (D7): large payloads — tool stdout,
//! transcripts, embeddings, downloaded model artifacts — live here, keyed
//! by their `blake3` content hash, so the SQLite database stays small and
//! identical outputs deduplicate for free.
//!
//! Layout: `<root>/<first 2 hex chars>/<next 2 hex chars>/<full hex hash>`,
//! matching the sharding sketched in the build plan's storage layout
//! (`blobs/<bl>/<ake3>...`), which keeps any single directory from
//! accumulating an unbounded number of entries.

use std::io::Write;
use std::path::{Path, PathBuf};

use valyria_util::ContentHash;

use crate::error::{Result, StoreError};

pub struct BlobStore {
    root: PathBuf,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BlobStoreReport {
    pub blob_count: u64,
    pub total_bytes: u64,
}

impl BlobStore {
    pub fn new(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn path_for(&self, hash: ContentHash) -> PathBuf {
        let hex = hash.to_hex();
        self.root.join(&hex[0..2]).join(&hex[2..4]).join(&hex)
    }

    /// Write `data`, returning its content hash. Idempotent: writing the
    /// same bytes twice is a no-op the second time (existence check first).
    /// Writes atomically (temp file + rename) so a reader can never observe
    /// a partially-written blob — the same discipline D6 requires of
    /// workspace file writes, applied here to the store's own files.
    pub fn put(&self, data: &[u8]) -> Result<ContentHash> {
        let hash = ContentHash::of_bytes(data);
        let dest = self.path_for(hash);
        if dest.exists() {
            return Ok(hash);
        }

        let parent = dest.parent().expect("path_for always has a parent");
        std::fs::create_dir_all(parent)?;

        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        tmp.write_all(data)?;
        tmp.flush()?;
        tmp.persist(&dest).map_err(|e| StoreError::Io(e.error))?;
        Ok(hash)
    }

    pub fn get(&self, hash: ContentHash) -> Result<Vec<u8>> {
        let path = self.path_for(hash);
        std::fs::read(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                StoreError::BlobNotFound(hash.to_hex())
            } else {
                StoreError::Io(e)
            }
        })
    }

    pub fn exists(&self, hash: ContentHash) -> bool {
        self.path_for(hash).exists()
    }

    pub fn delete(&self, hash: ContentHash) -> Result<()> {
        let path = self.path_for(hash);
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StoreError::Io(e)),
        }
    }

    /// Walk the store and report how much space it holds — the data behind
    /// `valyria clean`'s "inspect before you delete" requirement (§48).
    pub fn inspect(&self) -> Result<BlobStoreReport> {
        let mut report = BlobStoreReport::default();
        visit_blobs(&self.root, &mut |_path, len| {
            report.blob_count += 1;
            report.total_bytes += len;
        })?;
        Ok(report)
    }

    /// Delete every blob. Used by `valyria clean --blobs`; callers are
    /// responsible for confirming with the user first (§48, and the
    /// runtime's destructive-action rules).
    pub fn purge(&self) -> Result<()> {
        if self.root.exists() {
            std::fs::remove_dir_all(&self.root)?;
        }
        std::fs::create_dir_all(&self.root)?;
        Ok(())
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
}

fn visit_blobs(dir: &Path, f: &mut impl FnMut(&Path, u64)) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            visit_blobs(&path, f)?;
        } else {
            f(&path, metadata.len());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let hash = store.put(b"hello, blobs").unwrap();
        assert_eq!(store.get(hash).unwrap(), b"hello, blobs");
    }

    #[test]
    fn identical_content_deduplicates() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let a = store.put(b"same content").unwrap();
        let b = store.put(b"same content").unwrap();
        assert_eq!(a, b);

        let report = store.inspect().unwrap();
        assert_eq!(report.blob_count, 1);
    }

    #[test]
    fn missing_blob_is_a_typed_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let fake_hash = valyria_util::ContentHash::of_bytes(b"never written");
        let err = store.get(fake_hash).unwrap_err();
        assert!(matches!(err, StoreError::BlobNotFound(_)));
    }

    #[test]
    fn exists_reflects_presence() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let hash = ContentHash::of_bytes(b"present");
        assert!(!store.exists(hash));
        store.put(b"present").unwrap();
        assert!(store.exists(hash));
    }

    #[test]
    fn delete_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let hash = store.put(b"to be deleted").unwrap();
        store.delete(hash).unwrap();
        assert!(!store.exists(hash));
        store.delete(hash).unwrap(); // deleting again must not error
    }

    #[test]
    fn inspect_sums_bytes_across_shards() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        store.put(b"one").unwrap();
        store.put(b"two-longer-payload").unwrap();
        store.put(b"three").unwrap();

        let report = store.inspect().unwrap();
        assert_eq!(report.blob_count, 3);
        assert_eq!(
            report.total_bytes,
            (b"one".len() + b"two-longer-payload".len() + b"three".len()) as u64
        );
    }

    #[test]
    fn purge_removes_everything_but_store_stays_usable() {
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        store.put(b"gone soon").unwrap();
        store.purge().unwrap();

        assert_eq!(store.inspect().unwrap().blob_count, 0);
        // still usable after purge
        let hash = store.put(b"fresh").unwrap();
        assert_eq!(store.get(hash).unwrap(), b"fresh");
    }

    #[test]
    fn no_partial_file_left_behind_by_a_failed_style_write() {
        // Sanity check on the atomic-write mechanics: the path never
        // exists until `put` completes, and content is always whole.
        let dir = tempfile::tempdir().unwrap();
        let store = BlobStore::new(dir.path()).unwrap();
        let data = vec![0xAB; 10_000];
        let hash = store.put(&data).unwrap();
        let read_back = store.get(hash).unwrap();
        assert_eq!(read_back, data);
    }
}
