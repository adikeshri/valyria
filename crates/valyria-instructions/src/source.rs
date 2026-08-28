//! [`InstructionSource`] — one discovered file — and [`InstructionSet`],
//! the ordered collection with any detected contradictions attached.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use valyria_types::Trust;

use crate::authority::Authority;
use crate::conflict::InstructionConflict;

/// One instruction file the runtime found and read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionSource {
    pub authority: Authority,
    /// Path as given, relative to the workspace root where possible so the
    /// value is stable across machines.
    pub origin: PathBuf,
    /// Trust level for prompt assembly. Always `authority.trust()`; stored
    /// explicitly so a serialized set is self-describing.
    pub trust: Trust,
    /// File contents, size-capped (see [`InstructionSource::truncated`]).
    pub body: String,
    /// `true` if the file on disk was larger than the configured cap and
    /// `body` holds only its head. Truncation is always at a line boundary.
    pub truncated: bool,
    /// The file's size on disk in bytes, before any capping.
    pub bytes_on_disk: u64,
    /// `(mtime_ms, len)` at read time, for cheap change detection.
    pub fingerprint: FileFingerprint,
}

impl InstructionSource {
    pub fn is_directive(&self) -> bool {
        self.authority.is_directive()
    }
}

/// Enough of a file's metadata to tell whether it changed since it was
/// read, without re-reading it (§4.18: "re-read on change").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub mtime_ms: i64,
    pub len: u64,
}

impl FileFingerprint {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        let mtime_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Self {
            mtime_ms,
            len: meta.len(),
        }
    }
}

/// A whole-set fingerprint: every source's path and its
/// [`FileFingerprint`], plus the paths that were *looked for and absent*
/// (so that a newly-created `AGENTS.md` counts as a change).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionFingerprint {
    pub present: Vec<(PathBuf, FileFingerprint)>,
    pub absent: Vec<PathBuf>,
}

impl InstructionFingerprint {
    /// Whether a later discovery would produce different inputs than this
    /// one — a file changed size/mtime, appeared, or disappeared.
    pub fn differs_from(&self, other: &InstructionFingerprint) -> bool {
        self != other
    }
}

/// The result of discovery: every source in authority order (highest
/// first), plus any contradictions found between them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionSet {
    pub sources: Vec<InstructionSource>,
    pub conflicts: Vec<InstructionConflict>,
    pub fingerprint: InstructionFingerprint,
}

impl InstructionSet {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Sources the runtime may act on (everything except advisory files),
    /// still in authority order.
    pub fn directives(&self) -> impl Iterator<Item = &InstructionSource> {
        self.sources.iter().filter(|s| s.is_directive())
    }

    /// Advisory sources (`README`, `CONTRIBUTING.md`) — mined for facts,
    /// never obeyed.
    pub fn advisory(&self) -> impl Iterator<Item = &InstructionSource> {
        self.sources.iter().filter(|s| !s.is_directive())
    }

    /// The highest-authority source, if any.
    pub fn top(&self) -> Option<&InstructionSource> {
        self.sources.first()
    }

    /// Total bytes of instruction body the set would contribute to a
    /// prompt (post-cap).
    pub fn total_body_bytes(&self) -> usize {
        self.sources.iter().map(|s| s.body.len()).sum()
    }
}

/// Make `origin` relative to `root` when it is underneath it; otherwise
/// leave it as-is (e.g. a user-config path outside the workspace).
pub(crate) fn relativize(path: &Path, root: &Path) -> PathBuf {
    path.strip_prefix(root)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(authority: Authority, body: &str) -> InstructionSource {
        InstructionSource {
            trust: authority.trust(),
            authority,
            origin: "x".into(),
            body: body.to_string(),
            truncated: false,
            bytes_on_disk: body.len() as u64,
            fingerprint: FileFingerprint {
                mtime_ms: 0,
                len: body.len() as u64,
            },
        }
    }

    #[test]
    fn directives_excludes_advisory() {
        let set = InstructionSet {
            sources: vec![src(Authority::Agents, "a"), src(Authority::Advisory, "b")],
            conflicts: vec![],
            fingerprint: InstructionFingerprint::default(),
        };
        assert_eq!(set.directives().count(), 1);
        assert_eq!(set.advisory().count(), 1);
    }

    #[test]
    fn fingerprint_differs_when_a_file_appears() {
        let a = InstructionFingerprint {
            present: vec![],
            absent: vec!["AGENTS.md".into()],
        };
        let b = InstructionFingerprint {
            present: vec![(
                "AGENTS.md".into(),
                FileFingerprint {
                    mtime_ms: 1,
                    len: 10,
                },
            )],
            absent: vec![],
        };
        assert!(a.differs_from(&b));
    }
}
