//! On-disk model store (§4.21). Owns `~/.valyria/models/<id>/`: the
//! resumable download, the whole-file integrity check, the post-install
//! probe, the `manifest.json`, and reclamation (`remove`, `gc`).
//!
//! The flow is deliberately **never silent and never partial-on-success**:
//! [`ModelStore::plan_install`] surfaces size + license + hardware fit, the
//! plan must be [`InstallPlan::confirm`]ed, the download resumes from a
//! `.part` file, and a hash mismatch deletes the file rather than leaving a
//! broken install behind.

use std::fs::{self, File};
use std::io::{BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use valyria_hardware::{fits, Fit, HardwareReport};
use valyria_model_registry::ModelCard;
use valyria_util::{CancellationToken, ContentHash};

use crate::error::{ModelStoreError, Result};
use crate::fetch::Fetcher;
use crate::manifest::{Manifest, MANIFEST_FILENAME};
use crate::probe::Prober;

/// Download chunk size. Small enough that a cancel is responsive, large
/// enough that per-request overhead is negligible for multi-GB files.
const CHUNK_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ModelStore {
    root: PathBuf,
}

/// What the user must acknowledge before a download starts (§4.21
/// "explicit confirmation"). Produced by [`ModelStore::plan_install`];
/// [`ModelStore::install`] refuses an unconfirmed one.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub card: ModelCard,
    pub download_bytes: u64,
    pub license_name: String,
    pub license_url: Option<String>,
    pub fit: Fit,
    pub destination: PathBuf,
    pub already_installed: bool,
    confirmed: bool,
}

impl InstallPlan {
    #[must_use]
    pub fn confirm(mut self) -> Self {
        self.confirmed = true;
        self
    }

    pub fn is_confirmed(&self) -> bool {
        self.confirmed
    }

    /// A one-line human summary for a CLI prompt.
    pub fn summary(&self) -> String {
        format!(
            "{} — {:.2} GB, license {}, fit {:?}{}",
            self.card.id,
            self.download_bytes as f64 / 1e9,
            self.license_name,
            self.fit,
            if self.already_installed {
                " (already installed)"
            } else {
                ""
            }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GcReport {
    pub removed: Vec<String>,
    pub freed_bytes: u64,
    /// Stray `.part` files from interrupted downloads that were swept.
    pub swept_partials: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageReport {
    pub model_count: usize,
    pub total_bytes: u64,
}

impl ModelStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn models_dir(&self) -> PathBuf {
        self.root.join("models")
    }

    fn model_dir(&self, id: &str) -> PathBuf {
        self.models_dir().join(id)
    }

    fn manifest_path(&self, id: &str) -> PathBuf {
        self.model_dir(id).join(MANIFEST_FILENAME)
    }

    pub fn is_installed(&self, id: &str) -> bool {
        self.manifest_path(id).is_file()
    }

    /// Ids of every model with a `manifest.json`, sorted.
    pub fn installed(&self) -> Result<Vec<String>> {
        let dir = self.models_dir();
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if self.is_installed(&id) {
                out.push(id);
            }
        }
        out.sort();
        Ok(out)
    }

    pub fn manifest(&self, id: &str) -> Result<Manifest> {
        if !self.is_installed(id) {
            return Err(ModelStoreError::NotInstalled { id: id.to_string() });
        }
        let text = fs::read_to_string(self.manifest_path(id))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn plan_install(&self, card: &ModelCard, hw: &HardwareReport) -> InstallPlan {
        InstallPlan {
            download_bytes: card.file_size_bytes,
            license_name: card.license_name.clone(),
            license_url: card.license_url.clone(),
            fit: fits(&card.requirement, hw),
            destination: self.model_dir(&card.id),
            already_installed: self.is_installed(&card.id),
            confirmed: false,
            card: card.clone(),
        }
    }

    pub async fn install<F: Fetcher, P: Prober>(
        &self,
        plan: &InstallPlan,
        fetcher: &F,
        prober: &P,
        cancel: &CancellationToken,
    ) -> Result<Manifest> {
        let id = plan.card.id.clone();
        if !plan.confirmed {
            return Err(ModelStoreError::Unconfirmed { id });
        }
        if self.is_installed(&id) {
            return Err(ModelStoreError::AlreadyInstalled { id });
        }

        let dir = self.model_dir(&id);
        fs::create_dir_all(&dir)?;
        let weights_name = weights_filename(&plan.card);
        let final_path = dir.join(&weights_name);
        let part_path = dir.join(format!("{weights_name}.part"));

        let head =
            fetcher
                .head(&plan.card.source_url)
                .await
                .map_err(|e| ModelStoreError::Download {
                    id: id.clone(),
                    detail: format!("HEAD failed: {e}"),
                })?;

        // Resume from an existing `.part`, unless the server can't range or
        // the partial is somehow already larger than the object.
        let mut offset = match fs::metadata(&part_path) {
            Ok(m) if head.supports_ranges && m.len() <= head.len => m.len(),
            Ok(_) => {
                fs::remove_file(&part_path)?;
                0
            }
            Err(_) => 0,
        };

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(offset == 0)
            .open(&part_path)?;
        file.seek(SeekFrom::Start(offset))?;

        while offset < head.len {
            if cancel.is_cancelled() {
                // Leave the `.part` behind so a retry resumes.
                return Err(ModelStoreError::Cancelled { id });
            }
            let end = (offset + CHUNK_BYTES).min(head.len);
            let bytes = fetcher
                .get_range(&plan.card.source_url, offset, end)
                .await
                .map_err(|e| ModelStoreError::Download {
                    id: id.clone(),
                    detail: e.to_string(),
                })?;
            if bytes.is_empty() {
                return Err(ModelStoreError::Download {
                    id,
                    detail: format!("server returned 0 bytes at offset {offset} of {}", head.len),
                });
            }
            file.write_all(&bytes)?;
            offset += bytes.len() as u64;
        }
        file.flush()?;
        drop(file);

        // Whole-file integrity check (§4.21). A mismatch is a hard failure
        // and the bytes are deleted — no broken install is left on disk.
        let actual = ContentHash::of_reader(BufReader::new(File::open(&part_path)?))?.to_hex();
        if actual != plan.card.content_hash {
            let _ = fs::remove_file(&part_path);
            return Err(ModelStoreError::IntegrityMismatch {
                id,
                expected: plan.card.content_hash.clone(),
                actual,
            });
        }

        fs::rename(&part_path, &final_path)?;
        let size_bytes = fs::metadata(&final_path)?.len();

        let probe =
            prober
                .probe(&final_path, &plan.card)
                .await
                .map_err(|e| ModelStoreError::Probe {
                    id: id.clone(),
                    detail: e.to_string(),
                })?;
        if !probe.loads {
            let _ = fs::remove_dir_all(&dir);
            return Err(ModelStoreError::Probe {
                id,
                detail: "model did not load during probe".into(),
            });
        }

        let manifest = Manifest {
            weights_file: weights_name,
            size_bytes,
            content_hash: actual,
            installed_at_ms: now_ms(),
            probe: Some(probe),
            card: plan.card.clone(),
        };
        write_atomic(
            &self.manifest_path(&id),
            serde_json::to_string_pretty(&manifest)?.as_bytes(),
        )?;
        tracing::info!(model = %id, bytes = size_bytes, "model installed");
        Ok(manifest)
    }

    /// Re-hash the weights on disk and compare to what the manifest
    /// recorded — the `doctor`-style check for silent corruption.
    pub fn verify_integrity(&self, id: &str) -> Result<()> {
        let manifest = self.manifest(id)?;
        let weights = self.model_dir(id).join(&manifest.weights_file);
        let actual = ContentHash::of_reader(BufReader::new(File::open(&weights)?))?.to_hex();
        if actual != manifest.content_hash {
            return Err(ModelStoreError::IntegrityMismatch {
                id: id.to_string(),
                expected: manifest.content_hash,
                actual,
            });
        }
        Ok(())
    }

    /// Delete a model's directory. Returns the bytes reclaimed.
    pub fn remove(&self, id: &str) -> Result<u64> {
        if !self.is_installed(id) {
            return Err(ModelStoreError::NotInstalled { id: id.to_string() });
        }
        let dir = self.model_dir(id);
        let freed = dir_size(&dir)?;
        fs::remove_dir_all(&dir)?;
        tracing::info!(model = %id, freed_bytes = freed, "model removed");
        Ok(freed)
    }

    /// Remove every installed model whose id is **not** in `keep`, and
    /// sweep stray `.part` files from interrupted downloads.
    pub fn gc(&self, keep: &[String]) -> Result<GcReport> {
        let mut report = GcReport {
            removed: Vec::new(),
            freed_bytes: 0,
            swept_partials: 0,
        };
        for id in self.installed()? {
            if !keep.iter().any(|k| k == &id) {
                report.freed_bytes += self.remove(&id)?;
                report.removed.push(id);
            }
        }
        // Sweep partials in surviving (or manifest-less) directories.
        if self.models_dir().is_dir() {
            for entry in fs::read_dir(self.models_dir())? {
                let entry = entry?;
                if !entry.file_type()?.is_dir() {
                    continue;
                }
                for f in fs::read_dir(entry.path())? {
                    let f = f?;
                    if f.path().extension().and_then(|e| e.to_str()) == Some("part") {
                        report.swept_partials +=
                            fs::metadata(f.path()).map(|m| m.len()).unwrap_or(0);
                        let _ = fs::remove_file(f.path());
                    }
                }
            }
        }
        Ok(report)
    }

    pub fn storage_report(&self) -> Result<StorageReport> {
        let ids = self.installed()?;
        let mut total = 0;
        for id in &ids {
            total += dir_size(&self.model_dir(id))?;
        }
        Ok(StorageReport {
            model_count: ids.len(),
            total_bytes: total,
        })
    }
}

fn weights_filename(card: &ModelCard) -> String {
    card.source_url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty() && s.contains('.'))
        .map(|s| s.to_string())
        .unwrap_or_else(|| format!("{}.gguf", card.id))
}

fn dir_size(dir: &Path) -> Result<u64> {
    let mut total = 0;
    if !dir.is_dir() {
        return Ok(0);
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            total += dir_size(&entry.path())?;
        } else {
            total += entry.metadata()?.len();
        }
    }
    Ok(total)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.flush()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
