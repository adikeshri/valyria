//! Workspace-rooted path resolution: the one place every filesystem access
//! must pass through. Canonicalizes, rejects `..`/absolute traversal, and
//! refuses a symlink that would resolve outside the workspace root — the
//! default policy called for in §4.4 and §49 (path traversal protection,
//! symlink handling).

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use crate::error::{Result, VfsError};

/// A canonicalized workspace root. Every [`WorkspaceRoot::resolve`] call is
/// checked against this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRoot(PathBuf);

impl WorkspaceRoot {
    pub fn new(path: impl AsRef<Path>) -> Result<Self> {
        let canon = std::fs::canonicalize(&path).map_err(|e| VfsError::Io {
            path: path.as_ref().display().to_string(),
            source: e,
        })?;
        Ok(Self(canon))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Resolve a workspace-relative path, rejecting traversal and symlink
    /// escapes. The target need not exist: for a path that doesn't exist
    /// yet (e.g. a file about to be written), the deepest *existing*
    /// ancestor is canonicalized — which resolves any symlinks along that
    /// real portion of the path — and checked for containment; the
    /// non-existent tail is then joined back on lexically (it was already
    /// validated traversal-free by normalization, so it cannot reintroduce
    /// an escape).
    pub fn resolve(&self, rel: impl AsRef<Path>) -> Result<PathBuf> {
        let rel = rel.as_ref();
        if rel.is_absolute() {
            return Err(VfsError::PathTraversal(rel.display().to_string()));
        }

        let normalized = valyria_util::path::normalize_relative(rel)
            .ok_or_else(|| VfsError::PathTraversal(rel.display().to_string()))?;

        let candidate = self.0.join(&normalized);
        let (existing_ancestor, tail) = deepest_existing_ancestor(&candidate);

        let canon_ancestor =
            std::fs::canonicalize(&existing_ancestor).map_err(|e| VfsError::Io {
                path: existing_ancestor.display().to_string(),
                source: e,
            })?;

        if !canon_ancestor.starts_with(&self.0) {
            return Err(VfsError::SymlinkEscape(rel.display().to_string()));
        }

        Ok(if tail.as_os_str().is_empty() {
            canon_ancestor
        } else {
            canon_ancestor.join(tail)
        })
    }
}

fn deepest_existing_ancestor(path: &Path) -> (PathBuf, PathBuf) {
    let mut ancestor = path.to_path_buf();
    let mut tail: Vec<OsString> = Vec::new();

    loop {
        if ancestor.exists() {
            break;
        }
        match ancestor.file_name() {
            Some(name) => {
                tail.push(name.to_owned());
                ancestor.pop();
            }
            None => break, // hit the filesystem root without finding an existing component
        }
    }

    tail.reverse();
    let tail_path: PathBuf = tail.into_iter().collect();
    (ancestor, tail_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_an_existing_file_within_root() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("src/lib.rs", "fn main() {}");
        let root = WorkspaceRoot::new(ws.path()).unwrap();

        let resolved = root.resolve("src/lib.rs").unwrap();
        assert_eq!(resolved, root.as_path().join("src/lib.rs"));
    }

    #[test]
    fn resolves_a_not_yet_existing_file_for_writes() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.mkdir("src");
        let root = WorkspaceRoot::new(ws.path()).unwrap();

        let resolved = root.resolve("src/new_file.rs").unwrap();
        assert_eq!(resolved, root.as_path().join("src/new_file.rs"));
    }

    #[test]
    fn rejects_absolute_paths() {
        let ws = valyria_testkit::TempWorkspace::new();
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let err = root.resolve("/etc/passwd").unwrap_err();
        assert!(matches!(err, VfsError::PathTraversal(_)));
    }

    #[test]
    fn rejects_dot_dot_traversal() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.mkdir("src");
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let err = root.resolve("src/../../outside.txt").unwrap_err();
        assert!(matches!(err, VfsError::PathTraversal(_)));
    }

    #[test]
    fn dot_dot_that_stays_inside_root_is_allowed() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("a/b/file.txt", "hi");
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let resolved = root.resolve("a/b/../b/file.txt").unwrap();
        assert_eq!(resolved, root.as_path().join("a/b/file.txt"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_that_escapes_the_root() {
        let ws = valyria_testkit::TempWorkspace::new();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "shh").unwrap();

        ws.mkdir("src");
        std::os::unix::fs::symlink(outside.path(), ws.full_path("src/escape")).unwrap();

        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let err = root.resolve("src/escape/secret.txt").unwrap_err();
        assert!(matches!(err, VfsError::SymlinkEscape(_)));
    }

    #[cfg(unix)]
    #[test]
    fn allows_a_symlink_that_stays_within_the_root() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("real/file.txt", "hi");
        std::os::unix::fs::symlink(ws.full_path("real"), ws.full_path("alias")).unwrap();

        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let resolved = root.resolve("alias/file.txt").unwrap();
        assert_eq!(resolved, root.as_path().join("real/file.txt"));
    }
}
