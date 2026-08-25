//! `FsGuard` (§21): a defense-in-depth filesystem check used alongside a
//! `ProcessLauncher`. Where the launcher confines an actual OS process,
//! `FsGuard` lets the runtime ask "is this path allowed?" *before*
//! spawning anything — e.g. to reject a tool call early with a clear
//! error rather than let it fail opaquely inside a sandboxed subprocess.

use std::path::{Path, PathBuf};

pub trait FsGuard: Send + Sync {
    fn allows_write(&self, path: &Path) -> bool;
}

/// Checks containment against a fixed set of canonicalized roots. Not a
/// replacement for `valyria-vfs::WorkspaceRoot::resolve` (which is the
/// authoritative traversal/symlink defense for the primary workspace) —
/// this is a secondary check scoped to whatever a sandbox profile allows,
/// which may be narrower (or, for an out-of-workspace grant, different
/// from) the workspace root itself.
pub struct AllowlistFsGuard {
    roots: Vec<PathBuf>,
}

impl AllowlistFsGuard {
    /// Roots are canonicalized at construction; a root that doesn't exist
    /// yet is kept as-is (best-effort — matches the profile/subpath
    /// canonicalization approach used by the platform launchers).
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let roots = roots
            .into_iter()
            .map(|r| std::fs::canonicalize(&r).unwrap_or(r))
            .collect();
        Self { roots }
    }
}

impl FsGuard for AllowlistFsGuard {
    fn allows_write(&self, path: &Path) -> bool {
        let canon = std::fs::canonicalize(path);
        let candidate = canon.as_deref().unwrap_or(path);
        self.roots.iter().any(|root| candidate.starts_with(root))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_paths_within_a_root() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("sub");
        std::fs::create_dir_all(&sub).unwrap();

        let guard = AllowlistFsGuard::new([dir.path().to_path_buf()]);
        assert!(guard.allows_write(&sub));
    }

    #[test]
    fn denies_paths_outside_every_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let guard = AllowlistFsGuard::new([dir.path().to_path_buf()]);
        assert!(!guard.allows_write(outside.path()));
    }

    #[test]
    fn works_with_multiple_roots() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let guard = AllowlistFsGuard::new([a.path().to_path_buf(), b.path().to_path_buf()]);
        assert!(guard.allows_write(a.path()));
        assert!(guard.allows_write(b.path()));
        assert!(!guard.allows_write(outside.path()));
    }
}
