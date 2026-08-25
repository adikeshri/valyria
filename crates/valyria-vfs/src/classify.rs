//! Binary/large-file detection (§4.4): keeps junk out of anything that
//! walks the repository for context or indexing.

use std::io::Read;
use std::path::Path;

use crate::error::{Result, VfsError};

/// A file bigger than this is treated as "large" by default — never loaded
/// wholesale into model context, and indexed structurally at most.
pub const DEFAULT_MAX_CONTEXT_FILE_BYTES: u64 = 1_000_000; // 1 MB

const SNIFF_BYTES: usize = 8000;

/// Git's own heuristic, in essence: a NUL byte in the first few KB almost
/// never appears in real text and is a reliable binary signal in practice.
pub fn looks_binary(sample: &[u8]) -> bool {
    sample.contains(&0)
}

pub fn looks_binary_file(path: &Path) -> Result<bool> {
    let mut file = std::fs::File::open(path).map_err(|e| io_err(path, e))?;
    let mut buf = [0u8; SNIFF_BYTES];
    let n = file.read(&mut buf).map_err(|e| io_err(path, e))?;
    Ok(looks_binary(&buf[..n]))
}

pub fn is_oversized(path: &Path, max_bytes: u64) -> Result<bool> {
    let meta = std::fs::metadata(path).map_err(|e| io_err(path, e))?;
    Ok(meta.len() > max_bytes)
}

fn io_err(path: &Path, source: std::io::Error) -> VfsError {
    VfsError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_not_binary() {
        assert!(!looks_binary(b"fn main() { println!(\"hi\"); }"));
    }

    #[test]
    fn nul_byte_marks_binary() {
        assert!(looks_binary(b"\x00\x01\x02\x03"));
    }

    #[test]
    fn empty_is_not_binary() {
        assert!(!looks_binary(b""));
    }

    #[test]
    fn looks_binary_file_reads_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let text_path = dir.path().join("a.rs");
        std::fs::write(&text_path, b"pub fn f() {}").unwrap();
        assert!(!looks_binary_file(&text_path).unwrap());

        let bin_path = dir.path().join("a.bin");
        std::fs::write(&bin_path, [0u8, 1, 2, 3, 255]).unwrap();
        assert!(looks_binary_file(&bin_path).unwrap());
    }

    #[test]
    fn oversized_check_respects_the_given_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.txt");
        std::fs::write(&path, vec![b'x'; 100]).unwrap();
        assert!(!is_oversized(&path, 200).unwrap());
        assert!(is_oversized(&path, 50).unwrap());
    }
}
