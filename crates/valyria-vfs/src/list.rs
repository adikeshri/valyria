//! `.gitignore`-aware directory traversal, via the `ignore` crate — the
//! same traversal logic `ripgrep` uses. This is what keeps
//! `target/`, `node_modules/`, and friends out of indexing and search by
//! default without the runtime maintaining its own ignore-pattern engine.

use std::path::{Path, PathBuf};

use crate::error::{Result, VfsError};

pub fn list_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    // `require_git(false)`: honor a `.gitignore` file's patterns even in a
    // workspace that hasn't been `git init`'d yet (e.g. a freshly
    // scaffolded project before its first commit) — the `ignore` crate's
    // default otherwise silently skips gitignore rules outside a real
    // git repository, which would be a surprising difference from what a
    // developer sees in their editor.
    for entry in ignore::WalkBuilder::new(root).require_git(false).build() {
        let entry = entry.map_err(|e| VfsError::Io {
            path: root.display().to_string(),
            source: std::io::Error::other(e.to_string()),
        })?;
        if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            out.push(entry.into_path());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_regular_files() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("src/lib.rs", "fn f() {}")
            .write("Cargo.toml", "[package]")
            .write("README.md", "# hi");

        let files = list_files(ws.path()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| {
                p.strip_prefix(ws.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert!(names.contains(&"src/lib.rs".to_string()));
        assert!(names.contains(&"Cargo.toml".to_string()));
        assert!(names.contains(&"README.md".to_string()));
    }

    #[test]
    fn respects_gitignore() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write(".gitignore", "ignored_dir/\n*.log\n")
            .write("kept.txt", "kept")
            .write("ignored_dir/file.txt", "should not appear")
            .write("noisy.log", "should not appear either");

        let files = list_files(ws.path()).unwrap();
        let names: Vec<String> = files
            .iter()
            .map(|p| {
                p.strip_prefix(ws.path())
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert!(names.contains(&"kept.txt".to_string()));
        assert!(!names.iter().any(|n| n.starts_with("ignored_dir")));
        assert!(!names.iter().any(|n| n.ends_with(".log")));
    }
}
