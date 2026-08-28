//! `file:` URI conversion.
//!
//! LSP speaks URIs; the rest of the runtime speaks workspace-relative
//! paths. The conversion is small, and small enough to get wrong quietly,
//! so it lives in one place with its edge cases pinned by tests.

use std::path::{Path, PathBuf};

/// Percent-encode a path into a `file:` URI.
///
/// Only the characters that would change the URI's meaning are encoded —
/// `#` (fragment), `?` (query), `%` (the escape itself), and space. A
/// language server that receives an over-encoded URI will usually fail to
/// match it against its own document store, so encoding conservatively is
/// safer than encoding everything.
pub fn path_to_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    let text = path.to_string_lossy();

    // Windows paths (`C:\src\lib.rs`) need a leading slash and forward
    // slashes: `file:///C:/src/lib.rs`.
    let normalized = text.replace('\\', "/");
    if !normalized.starts_with('/') {
        out.push('/');
    }

    for ch in normalized.chars() {
        match ch {
            '%' => out.push_str("%25"),
            ' ' => out.push_str("%20"),
            '#' => out.push_str("%23"),
            '?' => out.push_str("%3F"),
            other => out.push(other),
        }
    }
    out
}

/// Parse a `file:` URI back into a path. Returns `None` for any other
/// scheme — servers legitimately send `untitled:` and `jdt:` URIs for
/// buffers and decompiled sources that have no file behind them, and
/// inventing a path for those would be worse than skipping the result.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let decoded = percent_decode(rest);

    // `file:///C:/src` -> `C:/src` on Windows; the leading slash before a
    // drive letter is URI syntax, not part of the path.
    #[cfg(windows)]
    let decoded = decoded
        .strip_prefix('/')
        .filter(|rest| {
            let bytes = rest.as_bytes();
            bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
        })
        .map(|rest| rest.to_string())
        .unwrap_or(decoded);

    Some(PathBuf::from(decoded))
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Some(byte) = hex_pair(bytes[i + 1], bytes[i + 2]) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_pair(high: u8, low: u8) -> Option<u8> {
    Some((hex_digit(high)? << 4) | hex_digit(low)?)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Express `path` relative to `root` when it is inside it, and as an
/// absolute path otherwise.
///
/// A definition outside the workspace — in a dependency's source — is a
/// legitimate answer, so it is returned rather than filtered out; it is
/// just not pretendable as a workspace path.
pub fn relative_to(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_unix_path_round_trips() {
        let path = Path::new("/home/dev/repo/src/lib.rs");
        let uri = path_to_uri(path);
        assert_eq!(uri, "file:///home/dev/repo/src/lib.rs");
        assert_eq!(uri_to_path(&uri).unwrap(), path);
    }

    #[test]
    fn a_path_with_a_space_round_trips() {
        let path = Path::new("/home/dev/my repo/src/lib.rs");
        let uri = path_to_uri(path);
        assert!(uri.contains("%20"));
        assert_eq!(uri_to_path(&uri).unwrap(), path);
    }

    #[test]
    fn a_path_containing_a_percent_sign_round_trips() {
        // Encoding `%` is what stops `100%.rs` from being decoded as an
        // escape sequence on the way back.
        let path = Path::new("/repo/100%.rs");
        let uri = path_to_uri(path);
        assert!(uri.contains("100%25"));
        assert_eq!(uri_to_path(&uri).unwrap(), path);
    }

    #[test]
    fn a_path_with_a_hash_round_trips() {
        let path = Path::new("/repo/c#/main.cs");
        assert_eq!(uri_to_path(&path_to_uri(path)).unwrap(), path);
    }

    #[test]
    fn a_non_file_scheme_has_no_path() {
        // Servers really do send these; a fabricated path would be worse
        // than no result.
        assert!(uri_to_path("untitled:Untitled-1").is_none());
        assert!(uri_to_path("jdt://contents/rt.jar").is_none());
    }

    #[test]
    fn a_truncated_escape_is_left_alone_rather_than_panicking() {
        assert_eq!(percent_decode("abc%"), "abc%");
        assert_eq!(percent_decode("abc%2"), "abc%2");
        assert_eq!(percent_decode("abc%zz"), "abc%zz");
    }

    #[test]
    fn paths_inside_the_workspace_come_back_relative() {
        let root = Path::new("/repo");
        assert_eq!(
            relative_to(root, Path::new("/repo/src/lib.rs")),
            "src/lib.rs"
        );
    }

    #[test]
    fn a_path_outside_the_workspace_stays_absolute() {
        let root = Path::new("/repo");
        assert_eq!(
            relative_to(root, Path::new("/home/.cargo/registry/serde/lib.rs")),
            "/home/.cargo/registry/serde/lib.rs"
        );
    }
}
