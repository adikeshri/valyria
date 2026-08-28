//! Bootstrap and incremental indexing (§4.14, §4.15) — the orchestration
//! that ties the scanner to the store.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use valyria_lang::LanguageRegistry;
use valyria_types::Generation;

use crate::error::Result;
use crate::record::{GenerationStage, IndexDelta, RelPath};
use crate::scan::{scan_paths, scan_workspace, ScanOptions, ScanProgress, ScannedFile};
use crate::store::{IndexStore, PublishOptions};

/// Progress through a bootstrap. `Staged` is the moment lexical search
/// becomes usable — §4.14's requirement that a 100k-file repository not be
/// unusable until the whole pipeline finishes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexProgress {
    Scanning(ScanProgress),
    /// The file list is published; symbols are still being written.
    Staged {
        generation: Generation,
        files: u64,
    },
    Complete {
        generation: Generation,
        files: u64,
        symbols: u64,
    },
}

/// The indexing pipeline: a scanner, a registry, and the store they feed.
#[derive(Debug)]
pub struct IndexPipeline {
    root: PathBuf,
    registry: LanguageRegistry,
    store: IndexStore,
    options: ScanOptions,
}

impl IndexPipeline {
    pub fn new(root: impl Into<PathBuf>, registry: LanguageRegistry, store: IndexStore) -> Self {
        Self {
            root: root.into(),
            registry,
            store,
            options: ScanOptions::default(),
        }
    }

    pub fn with_options(mut self, options: ScanOptions) -> Self {
        self.options = options;
        self
    }

    pub fn store(&self) -> &IndexStore {
        &self.store
    }

    pub fn registry(&self) -> &LanguageRegistry {
        &self.registry
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Index the whole workspace from scratch.
    ///
    /// Two generations, not one: the first carries files, hashes and
    /// languages, and is published as soon as the walk finishes; the
    /// second adds symbols. On a large repository the gap between them is
    /// most of the wall-clock time, and during it path and content search
    /// already work.
    ///
    /// The scan itself happens once — staging is about when results become
    /// *visible*, not about reading the tree twice.
    pub async fn bootstrap(&self, progress: &(dyn Fn(IndexProgress) + Sync)) -> Result<IndexDelta> {
        let scanned = self.scan_all(progress);

        let files_only: Vec<ScannedFile> = scanned
            .iter()
            .map(|file| ScannedFile {
                record: file.record.clone(),
                facts: Default::default(),
            })
            .collect();

        let staged = self
            .store
            .write_generation(
                files_only,
                Vec::new(),
                PublishOptions::full().stage(GenerationStage::FilesOnly),
            )
            .await?;
        progress(IndexProgress::Staged {
            generation: staged.generation,
            files: staged.added.len() as u64,
        });

        // `force_rewrite`: the bytes are identical to what stage one
        // published, so a hash comparison would call every file unchanged
        // and skip it — but the rows must be rewritten to carry the
        // symbols stage one did not extract.
        let complete = self
            .store
            .write_generation(scanned, Vec::new(), PublishOptions::full().force_rewrite())
            .await?;

        let stats = self.store.stats(complete.generation).await?;
        progress(IndexProgress::Complete {
            generation: complete.generation,
            files: stats.files,
            symbols: stats.symbols,
        });

        Ok(complete)
    }

    /// Index the whole workspace as a single generation, skipping the
    /// intermediate files-only publish. The right choice for a small
    /// repository and for the drift check, where the staged generation
    /// would only be noise.
    pub async fn bootstrap_unstaged(
        &self,
        progress: &(dyn Fn(IndexProgress) + Sync),
    ) -> Result<IndexDelta> {
        let scanned = self.scan_all(progress);
        let delta = self
            .store
            .write_generation(scanned, Vec::new(), PublishOptions::full())
            .await?;
        let stats = self.store.stats(delta.generation).await?;
        progress(IndexProgress::Complete {
            generation: delta.generation,
            files: stats.files,
            symbols: stats.symbols,
        });
        Ok(delta)
    }

    /// Apply a set of changed paths, producing a new generation.
    ///
    /// Paths that no longer exist on disk are treated as deletions, so a
    /// caller does not have to classify watcher events itself — and cannot
    /// get that classification wrong in a way that leaves the index
    /// claiming a deleted file still exists.
    pub async fn apply_paths(&self, paths: &[RelPath]) -> Result<IndexDelta> {
        let unique: BTreeSet<RelPath> = paths.iter().cloned().collect();
        let (present, absent): (Vec<RelPath>, Vec<RelPath>) = unique
            .into_iter()
            .partition(|rel| self.root.join(rel).is_file());

        let scanned = scan_paths(&self.root, &present, &self.registry, self.options);
        // A path that vanished between the partition and the scan (a
        // rebase mid-update, say) is a deletion too.
        let scanned_paths: BTreeSet<&str> =
            scanned.iter().map(|f| f.record.path.as_str()).collect();
        let mut removed = absent;
        removed.extend(
            present
                .iter()
                .filter(|p| !scanned_paths.contains(p.as_str()))
                .cloned(),
        );

        self.store
            .write_generation(scanned, removed, PublishOptions::incremental())
            .await
    }

    /// Re-walk the tree and reconcile in one generation.
    ///
    /// This is the recovery path for the bulk changes §4.15 calls out —
    /// a branch switch or rebase touching 5,000 files, where replaying
    /// individual watcher events is both slower and less reliable than
    /// looking at the result.
    pub async fn resync(&self) -> Result<IndexDelta> {
        let scanned = self.scan_all(&|_| {});
        self.store
            .write_generation(scanned, Vec::new(), PublishOptions::full())
            .await
    }

    pub(crate) fn scan_all(&self, progress: &(dyn Fn(IndexProgress) + Sync)) -> Vec<ScannedFile> {
        scan_workspace(&self.root, &self.registry, self.options, &|p| {
            progress(IndexProgress::Scanning(p))
        })
    }
}
