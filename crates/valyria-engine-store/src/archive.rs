//! Unpack a downloaded engine archive. Synchronous (like the rest of this
//! crate's disk work — `install_with_progress` already does the whole-file
//! blake3 check inline, blocking) and deliberately dumb: extract
//! everything under `dest`, don't try to cherry-pick members, because the
//! `llama-server` binary depends on sibling `.dylib`/`.so`/`.dll` files
//! shipped in the same archive and must keep running next to them.

use std::fs;
use std::io::Read;
use std::path::Path;

use crate::catalog::ArchiveKind;
use crate::error::{EngineStoreError, Result};

pub fn unpack(
    kind: ArchiveKind,
    archive_bytes: &[u8],
    dest: &Path,
    component: &str,
    version: &str,
) -> Result<()> {
    fs::create_dir_all(dest)?;
    let outcome = match kind {
        ArchiveKind::TarGz => unpack_tar_gz(archive_bytes, dest),
        ArchiveKind::Zip => unpack_zip(archive_bytes, dest),
    };
    outcome.map_err(|detail| EngineStoreError::Unpack {
        component: component.to_string(),
        version: version.to_string(),
        detail,
    })
}

fn unpack_tar_gz(bytes: &[u8], dest: &Path) -> std::result::Result<(), String> {
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive.unpack(dest).map_err(|e| e.to_string())
}

fn unpack_zip(bytes: &[u8], dest: &Path) -> std::result::Result<(), String> {
    let reader = std::io::Cursor::new(bytes);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let Some(name) = entry.enclosed_name() else {
            continue; // refuse a path-traversal / absolute entry name
        };
        let out_path = dest.join(name);
        if entry.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut out = fs::File::create(&out_path).map_err(|e| e.to_string())?;
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).map_err(|e| e.to_string())?;
        std::io::Write::write_all(&mut out, &buf).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = entry.unix_mode() {
                let _ = fs::set_permissions(&out_path, fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

/// Make `path` executable on unix. A no-op on other platforms (archives
/// there don't carry the executable bit the same way; `.exe` needs none).
#[cfg(unix)]
pub fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = fs::metadata(path)?.permissions();
    perm.set_mode(perm.mode() | 0o111);
    fs::set_permissions(path, perm)?;
    Ok(())
}

#[cfg(not(unix))]
pub fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Strip the `com.apple.quarantine` extended attribute macOS stamps on
/// anything downloaded by a network client (curl included — Core is not
/// special). Left in place, the *first* launch of an ad-hoc-signed binary
/// triggers an online Gatekeeper assessment that can take on the order of
/// a minute and looks, from the app, exactly like a hung server. Recursive
/// over `dir` because the release also ships sibling `.dylib`s that get
/// loaded (and therefore also quarantine-checked) at process start.
/// Best-effort and silent: a missing `xattr` binary or an attribute that
/// was never set is not a reason to fail the install.
#[cfg(target_os = "macos")]
pub fn strip_quarantine(dir: &Path) {
    let _ = std::process::Command::new("xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(dir)
        .status();
}

#[cfg(not(target_os = "macos"))]
pub fn strip_quarantine(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar_bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut tar_bytes);
            for (name, data) in entries {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                builder.append_data(&mut header, name, *data).unwrap();
            }
            builder.finish().unwrap();
        }
        let mut gz = Vec::new();
        {
            let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            enc.write_all(&tar_bytes).unwrap();
            enc.finish().unwrap();
        }
        gz
    }

    #[test]
    fn tar_gz_round_trips_a_binary_and_a_sibling_lib() {
        let dir = tempfile::tempdir().unwrap();
        let archive = make_tar_gz(&[
            ("llama-b1/llama-server", b"binary bytes"),
            ("llama-b1/libggml.dylib", b"lib bytes"),
        ]);
        unpack(ArchiveKind::TarGz, &archive, dir.path(), "llama.cpp", "b1").unwrap();
        assert_eq!(
            fs::read(dir.path().join("llama-b1/llama-server")).unwrap(),
            b"binary bytes"
        );
        assert_eq!(
            fs::read(dir.path().join("llama-b1/libggml.dylib")).unwrap(),
            b"lib bytes"
        );
    }

    #[test]
    fn zip_round_trips_a_binary() {
        let dir = tempfile::tempdir().unwrap();
        let mut zip_bytes = Vec::new();
        {
            let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut zip_bytes));
            w.start_file::<_, ()>("llama-server.exe", zip::write::SimpleFileOptions::default())
                .unwrap();
            w.write_all(b"exe bytes").unwrap();
            w.finish().unwrap();
        }
        unpack(ArchiveKind::Zip, &zip_bytes, dir.path(), "llama.cpp", "b1").unwrap();
        assert_eq!(
            fs::read(dir.path().join("llama-server.exe")).unwrap(),
            b"exe bytes"
        );
    }
}
