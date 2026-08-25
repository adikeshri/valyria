//! Atomic writes (D6): temp file in the same directory, then rename. A
//! reader can never observe a partially-written file. Preserves the
//! existing file's permission bits on unix, since a naive
//! `tempfile + rename` silently resets them to the umask default otherwise.

use std::io::Write;
use std::path::Path;

use crate::error::{Result, VfsError};

pub fn write_atomic(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| VfsError::Io {
        path: path.display().to_string(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent"),
    })?;
    std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;

    let existing_mode = existing_unix_mode(path);

    let mut tmp = tempfile::NamedTempFile::new_in(parent).map_err(|e| io_err(parent, e))?;
    tmp.write_all(contents).map_err(|e| io_err(path, e))?;
    tmp.flush().map_err(|e| io_err(path, e))?;

    #[cfg(unix)]
    if let Some(mode) = existing_mode {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(mode))
            .map_err(|e| io_err(path, e))?;
    }
    #[cfg(not(unix))]
    let _ = existing_mode;

    tmp.persist(path).map_err(|e| VfsError::Io {
        path: path.display().to_string(),
        source: e.error,
    })?;
    Ok(())
}

fn io_err(path: &Path, source: std::io::Error) -> VfsError {
    VfsError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(unix)]
fn existing_unix_mode(path: &Path) -> Option<u32> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).ok().map(|m| m.permissions().mode())
}

#[cfg(not(unix))]
fn existing_unix_mode(_path: &Path) -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("new.txt");
        write_atomic(&path, b"hello").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
    }

    #[test]
    fn creates_missing_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/c/deep.txt");
        write_atomic(&path, b"deep").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"deep");
    }

    #[test]
    fn overwrites_existing_file_wholly() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"old content, quite long").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn preserves_existing_permission_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, b"old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();

        write_atomic(&path, b"new content").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640);
    }

    #[test]
    fn never_leaves_a_temp_file_behind_on_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        write_atomic(&path, b"content").unwrap();

        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("f.txt")]);
    }
}
