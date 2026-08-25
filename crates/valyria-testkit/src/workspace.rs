//! A disposable temporary workspace for tests that need real files on
//! disk — the VFS, ledger, editing-engine, and repository-index test
//! suites all build fixture repos on top of this rather than each
//! reimplementing tempdir bookkeeping.

use std::path::{Path, PathBuf};

pub struct TempWorkspace {
    dir: tempfile::TempDir,
}

impl TempWorkspace {
    pub fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("failed to create temp workspace"),
        }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn full_path(&self, rel: impl AsRef<Path>) -> PathBuf {
        self.dir.path().join(rel)
    }

    /// Write `content` to `rel`, creating parent directories as needed.
    /// Returns `&Self` so fixture setup reads as a chain:
    /// `TempWorkspace::new().write("src/lib.rs", "..").write("Cargo.toml", "..")`.
    pub fn write(&self, rel: impl AsRef<Path>, content: impl AsRef<[u8]>) -> &Self {
        let path = self.full_path(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::write(&path, content).expect("failed to write fixture file");
        self
    }

    pub fn mkdir(&self, rel: impl AsRef<Path>) -> &Self {
        std::fs::create_dir_all(self.full_path(rel)).expect("failed to create fixture dir");
        self
    }

    pub fn read(&self, rel: impl AsRef<Path>) -> String {
        std::fs::read_to_string(self.full_path(&rel)).unwrap_or_else(|e| {
            panic!(
                "failed to read fixture file {}: {e}",
                rel.as_ref().display()
            )
        })
    }

    pub fn exists(&self, rel: impl AsRef<Path>) -> bool {
        self.full_path(rel).exists()
    }

    pub fn remove(&self, rel: impl AsRef<Path>) {
        std::fs::remove_file(self.full_path(rel)).expect("failed to remove fixture file");
    }
}

impl Default for TempWorkspace {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_then_read_round_trips() {
        let ws = TempWorkspace::new();
        ws.write("src/lib.rs", "fn main() {}");
        assert_eq!(ws.read("src/lib.rs"), "fn main() {}");
    }

    #[test]
    fn write_creates_nested_parent_dirs() {
        let ws = TempWorkspace::new();
        ws.write("a/b/c/d.txt", "deep");
        assert!(ws.exists("a/b/c/d.txt"));
        assert!(ws.full_path("a/b/c").is_dir());
    }

    #[test]
    fn chained_writes_build_a_fixture_tree() {
        let ws = TempWorkspace::new();
        ws.write("Cargo.toml", "[package]\nname = \"x\"\n")
            .write("src/lib.rs", "pub fn x() {}")
            .write("README.md", "# x\n");

        assert!(ws.exists("Cargo.toml"));
        assert!(ws.exists("src/lib.rs"));
        assert!(ws.exists("README.md"));
    }

    #[test]
    fn each_workspace_is_isolated() {
        let a = TempWorkspace::new();
        let b = TempWorkspace::new();
        a.write("only-in-a.txt", "a");
        assert!(!b.exists("only-in-a.txt"));
        assert_ne!(a.path(), b.path());
    }

    #[test]
    fn remove_deletes_a_file() {
        let ws = TempWorkspace::new();
        ws.write("gone.txt", "bye");
        ws.remove("gone.txt");
        assert!(!ws.exists("gone.txt"));
    }
}
