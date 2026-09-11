//! On-disk engine store (mirrors `valyria-model-store::ModelStore`, same
//! "never silent, never partial-on-success" discipline). Owns
//! `~/.valyria/engines/<component>/<release>/`: the resumable download of
//! the release archive, the whole-file integrity check, the unpack, and a
//! small manifest so [`EngineStore::resolve`] never re-downloads.

use std::fs::{self, File};
use std::io::{BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use valyria_util::{CancellationToken, ContentHash};

use crate::archive::{make_executable, strip_quarantine, unpack};
use crate::catalog::Catalog;
use crate::error::{EngineStoreError, Result};
use crate::fetch::Fetcher;

const CHUNK_BYTES: u64 = 4 * 1024 * 1024;
const MANIFEST_FILENAME: &str = "engine-manifest.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPhase {
    Downloading,
    Verifying,
    Unpacking,
}

impl InstallPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            InstallPhase::Downloading => "downloading",
            InstallPhase::Verifying => "verifying",
            InstallPhase::Unpacking => "unpacking",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallProgress {
    pub phase: InstallPhase,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct EngineManifest {
    component: String,
    release: String,
    /// The `llama-server` (or equivalent) binary path, relative to this
    /// manifest's own directory.
    binary_member: String,
    installed_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct EngineStore {
    root: PathBuf,
}

impl EngineStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    fn engines_dir(&self) -> PathBuf {
        self.root.join("engines")
    }

    fn component_dir(&self, component: &str) -> PathBuf {
        self.engines_dir().join(component)
    }

    fn release_dir(&self, component: &str, release: &str) -> PathBuf {
        self.component_dir(component).join(release)
    }

    /// The binary for `component` already on disk, if any release has
    /// finished installing — independent of which release the catalog
    /// currently pins, so a catalog bump doesn't force a redundant
    /// download before the caller explicitly asks to upgrade.
    pub fn resolve(&self, component: &str) -> Option<PathBuf> {
        let dir = self.component_dir(component);
        let mut releases: Vec<PathBuf> = fs::read_dir(&dir)
            .ok()?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .map(|e| e.path())
            .collect();
        // Newest install wins if more than one release happens to be
        // present (e.g. after a manual catalog bump).
        releases.sort_by_key(|p| {
            fs::metadata(p)
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
        });
        for dir in releases.into_iter().rev() {
            let manifest_path = dir.join(MANIFEST_FILENAME);
            let Ok(text) = fs::read_to_string(&manifest_path) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_str::<EngineManifest>(&text) else {
                continue;
            };
            let bin = dir.join(&manifest.binary_member);
            if bin.is_file() {
                return Some(bin);
            }
        }
        None
    }

    /// Download, verify, and unpack `component`'s catalog-pinned release
    /// for the current `(std::env::consts::OS, std::env::consts::ARCH)`.
    /// Idempotent — a second call when [`Self::resolve`] would already
    /// succeed returns immediately without touching the network.
    pub async fn install_with_progress<F: Fetcher>(
        &self,
        catalog: &Catalog,
        component: &str,
        fetcher: &F,
        cancel: &CancellationToken,
        progress: &(dyn Fn(InstallProgress) + Sync),
    ) -> Result<PathBuf> {
        if let Some(bin) = self.resolve(component) {
            return Ok(bin);
        }

        let entry =
            catalog
                .entry(component)
                .ok_or_else(|| EngineStoreError::UnsupportedTarget {
                    component: component.to_string(),
                    os: std::env::consts::OS.to_string(),
                    arch: std::env::consts::ARCH.to_string(),
                })?;
        let target = catalog.target_for(component, std::env::consts::OS, std::env::consts::ARCH)?;
        let release = &entry.release;

        let component_dir = self.component_dir(component);
        fs::create_dir_all(&component_dir)?;
        let archive_path = component_dir.join(format!("{release}.download"));
        let part_path = component_dir.join(format!("{release}.download.part"));

        let head = fetcher
            .head(&target.url)
            .await
            .map_err(|e| EngineStoreError::Download {
                component: component.to_string(),
                version: release.clone(),
                detail: e.to_string(),
            })?;

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
                return Err(EngineStoreError::Cancelled {
                    component: component.to_string(),
                    version: release.clone(),
                });
            }
            let end = (offset + CHUNK_BYTES).min(head.len);
            let bytes = fetcher
                .get_range(&target.url, offset, end)
                .await
                .map_err(|e| EngineStoreError::Download {
                    component: component.to_string(),
                    version: release.clone(),
                    detail: e.to_string(),
                })?;
            if bytes.is_empty() {
                return Err(EngineStoreError::Download {
                    component: component.to_string(),
                    version: release.clone(),
                    detail: format!("server returned 0 bytes at offset {offset} of {}", head.len),
                });
            }
            file.write_all(&bytes)?;
            offset += bytes.len() as u64;
            progress(InstallProgress {
                phase: InstallPhase::Downloading,
                downloaded_bytes: offset,
                total_bytes: head.len,
            });
        }
        file.flush()?;
        drop(file);

        progress(InstallProgress {
            phase: InstallPhase::Verifying,
            downloaded_bytes: head.len,
            total_bytes: head.len,
        });
        let actual = ContentHash::of_reader(BufReader::new(File::open(&part_path)?))?.to_hex();
        if actual != target.archive_hash {
            let _ = fs::remove_file(&part_path);
            return Err(EngineStoreError::IntegrityMismatch {
                component: component.to_string(),
                version: release.clone(),
                expected: target.archive_hash.clone(),
                actual,
            });
        }
        fs::rename(&part_path, &archive_path)?;

        progress(InstallProgress {
            phase: InstallPhase::Unpacking,
            downloaded_bytes: head.len,
            total_bytes: head.len,
        });
        let release_dir = self.release_dir(component, release);
        let archive_bytes = fs::read(&archive_path)?;
        unpack(
            target.archive_kind,
            &archive_bytes,
            &release_dir,
            component,
            release,
        )?;
        let _ = fs::remove_file(&archive_path);

        let bin_path = release_dir.join(&target.binary_member);
        if !bin_path.is_file() {
            let _ = fs::remove_dir_all(&release_dir);
            return Err(EngineStoreError::MissingBinary {
                component: component.to_string(),
                version: release.clone(),
                member: target.binary_member.clone(),
            });
        }
        make_executable(&bin_path)?;
        // Unquarantine the whole release dir (binary + its sibling
        // libraries), not just the binary — see `strip_quarantine`'s doc.
        strip_quarantine(&release_dir);

        let manifest = EngineManifest {
            component: component.to_string(),
            release: release.clone(),
            binary_member: target.binary_member.clone(),
            installed_at_ms: now_ms(),
        };
        write_atomic(
            &release_dir.join(MANIFEST_FILENAME),
            serde_json::to_string_pretty(&manifest)?.as_bytes(),
        )?;
        tracing::info!(component, release, "engine installed");
        Ok(bin_path)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{ArchiveKind, EngineEntry, EngineTarget};
    use crate::fetch::InMemoryFetcher;

    fn test_catalog(archive: &[u8], hash: &str) -> Catalog {
        Catalog::from_engines(vec![EngineEntry {
            component: "llama.cpp".to_string(),
            release: "t1".to_string(),
            targets: vec![EngineTarget {
                os: std::env::consts::OS.to_string(),
                arch: std::env::consts::ARCH.to_string(),
                url: "https://example.invalid/llama.tar.gz".to_string(),
                archive_kind: ArchiveKind::TarGz,
                archive_size_bytes: archive.len() as u64,
                archive_hash: hash.to_string(),
                binary_member: "t1/llama-server".to_string(),
            }],
        }])
    }

    fn make_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write as _;
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (name, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, name, *data).unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            enc.write_all(&tar_bytes).unwrap();
            enc.finish().unwrap();
        }
        gz
    }

    #[tokio::test]
    async fn install_then_resolve_round_trips() {
        let archive = make_tar_gz(&[("t1/llama-server", b"binary bytes")]);
        let hash = ContentHash::of_bytes(&archive).to_string();
        let catalog = test_catalog(&archive, &hash);

        let dir = tempfile::tempdir().unwrap();
        let store = EngineStore::new(dir.path());
        let fetcher =
            InMemoryFetcher::new().with_object("https://example.invalid/llama.tar.gz", archive);
        let cancel = CancellationToken::new();

        assert!(store.resolve("llama.cpp").is_none());
        let bin = store
            .install_with_progress(&catalog, "llama.cpp", &fetcher, &cancel, &|_| {})
            .await
            .unwrap();
        assert!(bin.ends_with("t1/llama-server"));
        assert_eq!(fs::read(&bin).unwrap(), b"binary bytes");

        // Idempotent: resolve now finds it, and a second install call
        // never touches the (now-empty) fetcher.
        assert_eq!(store.resolve("llama.cpp"), Some(bin.clone()));
        let empty_fetcher = InMemoryFetcher::new();
        let bin2 = store
            .install_with_progress(&catalog, "llama.cpp", &empty_fetcher, &cancel, &|_| {})
            .await
            .unwrap();
        assert_eq!(bin2, bin);
    }

    #[tokio::test]
    async fn hash_mismatch_deletes_the_partial_and_errors() {
        let archive = make_tar_gz(&[("t1/llama-server", b"binary bytes")]);
        let catalog = test_catalog(&archive, &"0".repeat(64));

        let dir = tempfile::tempdir().unwrap();
        let store = EngineStore::new(dir.path());
        let fetcher =
            InMemoryFetcher::new().with_object("https://example.invalid/llama.tar.gz", archive);
        let cancel = CancellationToken::new();

        let err = store
            .install_with_progress(&catalog, "llama.cpp", &fetcher, &cancel, &|_| {})
            .await
            .unwrap_err();
        assert!(matches!(err, EngineStoreError::IntegrityMismatch { .. }));
        assert!(store.resolve("llama.cpp").is_none());
    }

    /// Real network, real GitHub release, real unpack — proves the pinned
    /// catalog entry for this machine's `(os, arch)` is genuinely
    /// fetchable and produces a runnable binary. `#[ignore]`d because it
    /// needs the network; run explicitly with `cargo test -p
    /// valyria-engine-store -- --ignored install_real_engine_and_it_runs`.
    #[cfg(feature = "http")]
    #[tokio::test]
    #[ignore]
    async fn install_real_engine_and_it_runs() {
        let catalog = Catalog::embedded().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let store = EngineStore::new(dir.path());
        let fetcher = crate::fetch::HttpFetcher::new().unwrap();
        let cancel = CancellationToken::new();

        let bin = store
            .install_with_progress(&catalog, "llama.cpp", &fetcher, &cancel, &|p| {
                eprintln!("{:?} {}/{}", p.phase, p.downloaded_bytes, p.total_bytes);
            })
            .await
            .expect("real install");

        let out = std::process::Command::new(&bin)
            .env("DYLD_LIBRARY_PATH", bin.parent().unwrap())
            .env("LD_LIBRARY_PATH", bin.parent().unwrap())
            .arg("--version")
            .output()
            .expect("run llama-server --version");
        assert!(
            out.status.success(),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
