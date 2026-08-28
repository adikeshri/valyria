//! `verify-index` (§4.15): rebuild from scratch and diff against what the
//! incremental pipeline believes.
//!
//! > Index drift is the classic silent failure in this class of system; we
//! > test for it explicitly.
//!
//! Drift has no symptom of its own. The agent simply gets slightly wrong
//! context and produces slightly wrong work, and nothing anywhere reports
//! an error. So the correctness insurance is a second, independent
//! computation — a full rescan — compared field by field against the
//! stored index. This runs in CI against fixture repositories and is
//! available to `doctor`.

use serde::{Deserialize, Serialize};
use valyria_types::Generation;

use crate::error::Result;
use crate::pipeline::IndexPipeline;
use crate::record::RelPath;

/// What a rescan disagrees with the stored index about. Empty means no
/// drift.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexDrift {
    pub generation: Generation,
    /// On disk, absent from the index.
    pub missing_files: Vec<RelPath>,
    /// In the index, gone from disk.
    pub stale_files: Vec<RelPath>,
    /// Present in both, but the index's content hash is out of date.
    pub stale_content: Vec<RelPath>,
    /// Present in both with matching content, but a different set of
    /// symbols — the subtlest and most damaging class, because the file
    /// looks up to date.
    pub symbol_mismatches: Vec<SymbolMismatch>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SymbolMismatch {
    pub path: RelPath,
    pub indexed: Vec<String>,
    pub actual: Vec<String>,
}

impl IndexDrift {
    pub fn is_clean(&self) -> bool {
        self.missing_files.is_empty()
            && self.stale_files.is_empty()
            && self.stale_content.is_empty()
            && self.symbol_mismatches.is_empty()
    }

    pub fn total(&self) -> usize {
        self.missing_files.len()
            + self.stale_files.len()
            + self.stale_content.len()
            + self.symbol_mismatches.len()
    }
}

/// Rescan the workspace and compare against `generation`.
///
/// The rescan is deliberately independent of the incremental pipeline: it
/// walks the tree and re-extracts, so a bug shared between the two cannot
/// hide the drift the check exists to find.
pub async fn verify_index(pipeline: &IndexPipeline, generation: Generation) -> Result<IndexDrift> {
    let actual = pipeline.scan_all(&|_| {});
    let store = pipeline.store();

    let indexed_files = store.files(generation).await?;
    let mut drift = IndexDrift {
        generation,
        ..Default::default()
    };

    let indexed: std::collections::BTreeMap<&str, &crate::record::FileRecord> =
        indexed_files.iter().map(|f| (f.path.as_str(), f)).collect();

    for file in &actual {
        let path = file.record.path.as_str();
        let Some(stored) = indexed.get(path) else {
            drift.missing_files.push(path.to_string());
            continue;
        };

        if stored.content_hash != file.record.content_hash {
            drift.stale_content.push(path.to_string());
            // Symbols are compared only for files whose content agrees:
            // otherwise every stale file would report a symbol mismatch
            // too, which is the same fact counted twice.
            continue;
        }

        let indexed_symbols = store.symbols_in(generation, path).await?;
        let mut indexed_paths: Vec<String> = indexed_symbols
            .iter()
            .map(|s| format!("{}:{}", s.kind.as_str(), s.symbol_path))
            .collect();
        let mut actual_paths: Vec<String> = file
            .facts
            .symbols
            .iter()
            .map(|s| format!("{}:{}", s.kind.as_str(), s.symbol_path))
            .collect();
        indexed_paths.sort();
        actual_paths.sort();

        if indexed_paths != actual_paths {
            drift.symbol_mismatches.push(SymbolMismatch {
                path: path.to_string(),
                indexed: indexed_paths,
                actual: actual_paths,
            });
        }
    }

    let on_disk: std::collections::BTreeSet<&str> =
        actual.iter().map(|f| f.record.path.as_str()).collect();
    for path in indexed.keys() {
        if !on_disk.contains(path) {
            drift.stale_files.push((*path).to_string());
        }
    }

    Ok(drift)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_drift_report_is_clean_and_counts_zero() {
        let drift = IndexDrift::default();
        assert!(drift.is_clean());
        assert_eq!(drift.total(), 0);
    }

    #[test]
    fn any_category_makes_a_report_dirty() {
        let drift = IndexDrift {
            stale_files: vec!["gone.rs".into()],
            ..Default::default()
        };
        assert!(!drift.is_clean());
        assert_eq!(drift.total(), 1);
    }
}
