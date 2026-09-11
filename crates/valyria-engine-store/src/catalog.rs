//! The embedded engine catalog (mirrors `valyria-model-registry`'s
//! `Catalog::embedded()` — a pinned, offline-available list of what can be
//! fetched, never a live index). One entry per `(component, release)`, one
//! target per `(os, arch)` it ships a build for.
//!
//! Bumping the pinned `release` is a deliberate PR: download the new
//! release's assets, blake3 them, and update `catalog.json` — the same
//! discipline `valyria-model-registry`'s `content_hash` values follow for
//! HuggingFace weights that also don't publish a machine-readable hash.

use serde::{Deserialize, Serialize};

use crate::error::{EngineStoreError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveKind {
    TarGz,
    Zip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineTarget {
    /// `std::env::consts::OS` value: `macos` | `linux` | `windows`.
    pub os: String,
    /// `std::env::consts::ARCH` value: `aarch64` | `x86_64`.
    pub arch: String,
    pub url: String,
    pub archive_kind: ArchiveKind,
    pub archive_size_bytes: u64,
    /// blake3 hex digest of the whole archive, computed once when the
    /// release was pinned (GitHub does not publish per-asset checksums).
    pub archive_hash: String,
    /// Path of the `llama-server` binary inside the unpacked archive,
    /// relative to the archive root.
    pub binary_member: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineEntry {
    pub component: String,
    pub release: String,
    pub targets: Vec<EngineTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct CatalogFile {
    version: u32,
    engines: Vec<EngineEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Catalog {
    engines: Vec<EngineEntry>,
}

const EMBEDDED_JSON: &str = include_str!("catalog.json");

impl Catalog {
    /// Test-only constructor — production code only ever gets a `Catalog`
    /// from [`Self::embedded`].
    #[cfg(test)]
    pub(crate) fn from_engines(engines: Vec<EngineEntry>) -> Self {
        Self { engines }
    }

    pub fn embedded() -> Result<Self> {
        let file: CatalogFile = serde_json::from_str(EMBEDDED_JSON)?;
        Ok(Self {
            engines: file.engines,
        })
    }

    pub fn entry(&self, component: &str) -> Option<&EngineEntry> {
        self.engines.iter().find(|e| e.component == component)
    }

    /// The target for `component` matching the given `(os, arch)` — pass
    /// `std::env::consts::OS` / `std::env::consts::ARCH` in production,
    /// something else in tests.
    pub fn target_for<'a>(
        &'a self,
        component: &str,
        os: &str,
        arch: &str,
    ) -> Result<&'a EngineTarget> {
        self.entry(component)
            .and_then(|e| e.targets.iter().find(|t| t.os == os && t.arch == arch))
            .ok_or_else(|| EngineStoreError::UnsupportedTarget {
                component: component.to_string(),
                os: os.to_string(),
                arch: arch.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_catalog_parses_and_has_llama_cpp() {
        let cat = Catalog::embedded().unwrap();
        let entry = cat.entry("llama.cpp").expect("llama.cpp entry");
        assert!(!entry.release.is_empty());
        assert!(!entry.targets.is_empty());
        for t in &entry.targets {
            assert_eq!(t.archive_hash.len(), 64, "blake3 hex digest is 64 chars");
            assert!(t.archive_size_bytes > 0);
            assert!(!t.binary_member.is_empty());
        }
    }

    #[test]
    fn target_for_resolves_known_triples() {
        let cat = Catalog::embedded().unwrap();
        assert!(cat.target_for("llama.cpp", "macos", "aarch64").is_ok());
        assert!(cat.target_for("llama.cpp", "linux", "x86_64").is_ok());
        assert!(cat.target_for("llama.cpp", "windows", "x86_64").is_ok());
    }

    #[test]
    fn target_for_unknown_triple_is_unsupported() {
        let cat = Catalog::embedded().unwrap();
        let err = cat.target_for("llama.cpp", "plan9", "riscv64").unwrap_err();
        assert!(err.to_string().contains("plan9"));
    }
}
