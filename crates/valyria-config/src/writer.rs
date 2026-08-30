//! Writing config leaves back to a Core-owned file (§4.3, G6).
//!
//! `config_show` is read-only by design; this is the paired writer the
//! `config_set` wire method needs. It edits one dotted leaf in a
//! `config.toml`, preserving every sibling value, and validates the result
//! against the policy floor *before* the file is touched — a write that
//! would loosen access past the compiled ceiling is rejected and nothing
//! changes on disk.
//!
//! Plain-`toml` read / modify / re-serialize: sibling *values* round-trip
//! intact; comments and hand-formatting in the file do not survive an edit.
//! That matches the app-side `valyria_bridge::config_writer` this method
//! supersedes.

use std::path::Path;

use toml::Value;

use crate::error::{ConfigError, Result};
use crate::floor::PolicyFloor;

/// The dotted keys `config_set` accepts. Each corresponds to a leaf
/// `config_show` reports and a field `Settings` can deserialize. Grows in
/// lockstep with [`Settings`] as owning subsystems land (§4.3).
pub const WRITABLE_KEYS: &[&str] = &[
    "permission.mode",
    "log.format",
    "network.repository",
    "network.workspace_filesystem",
    "network.local_commands",
    "network.internet",
    "network.credentials",
];

/// Whether [`write_key`] will accept `key`.
pub fn is_writable_key(key: &str) -> bool {
    WRITABLE_KEYS.contains(&key)
}

/// Set `key` to `value` in the TOML file at `path`, creating the file (and
/// any missing parent directory) if absent, using the default policy floor.
///
/// Errors:
/// - [`ConfigError::UnknownKey`] — `key` is not in [`WRITABLE_KEYS`];
/// - [`ConfigError::Deserialize`] — `value` is not valid for that key
///   (e.g. `permission.mode = "turbo"`);
/// - [`ConfigError::PolicyFloorViolation`] — the resulting settings breach
///   the floor. In every error case the file on disk is left untouched.
pub fn write_key(path: &Path, key: &str, value: &str) -> Result<()> {
    write_key_with_floor(path, key, value, &PolicyFloor::default())
}

/// [`write_key`] with an explicit floor (tests use a looser one to prove
/// the gate is the floor and not an incidental parse failure).
pub fn write_key_with_floor(
    path: &Path,
    key: &str,
    value: &str,
    floor: &PolicyFloor,
) -> Result<()> {
    if !is_writable_key(key) {
        return Err(ConfigError::UnknownKey {
            key: key.to_string(),
        });
    }

    let mut doc: Value = if path.exists() {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        toml::from_str(&text).map_err(|e| ConfigError::Toml {
            path: path.display().to_string(),
            source: e,
        })?
    } else {
        Value::Table(Default::default())
    };

    set_dotted(&mut doc, key, parse_scalar(value));

    // Validate by layering the candidate document over the compiled
    // defaults — the same machinery `config_show` resolves through — and
    // running the floor check. A bare `toml::from_str::<Settings>` on the
    // candidate alone would spuriously fail on a partial `[network]` table
    // (its fields have no per-field `#[serde(default)]`); resolving over
    // the default layer fills the siblings correctly. `resolve()` also
    // rejects an invalid value (`config.deserialize`) and a floor breach
    // (`config.policy_floor_violation`) itself, so nothing reaches disk in
    // either case.
    let candidate_str =
        toml::to_string(&doc).map_err(|source| ConfigError::Serialize { source })?;
    crate::resolver::ConfigResolver::new()
        .override_toml(candidate_str.clone())
        .env_vars(Vec::new())
        .floor(*floor)
        .resolve()?;

    // Atomic replace: write a sibling temp file, then rename over the
    // target so a crash mid-write can never leave a half-written config.
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| ConfigError::Io {
            path: parent.display().to_string(),
            source: e,
        })?;
    }
    let tmp = path.with_extension(format!("toml.tmp.{}", std::process::id()));
    std::fs::write(&tmp, candidate_str.as_bytes()).map_err(|e| ConfigError::Io {
        path: tmp.display().to_string(),
        source: e,
    })?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        ConfigError::Io {
            path: path.display().to_string(),
            source: e,
        }
    })?;
    Ok(())
}

/// Best-effort scalar typing: `true`/`false` → bool, a bare integer → int,
/// otherwise a string. Every current writable key is a string enum; typing
/// bools and ints now keeps the writer correct as `Settings` grows leaves
/// of those types.
fn parse_scalar(raw: &str) -> Value {
    match raw {
        "true" => return Value::Boolean(true),
        "false" => return Value::Boolean(false),
        _ => {}
    }
    if let Ok(i) = raw.parse::<i64>() {
        return Value::Integer(i);
    }
    Value::String(raw.to_string())
}

/// Insert `leaf` at `dotted` (`a.b.c`), creating intermediate tables and
/// replacing any non-table value found along the path.
fn set_dotted(doc: &mut Value, dotted: &str, leaf: Value) {
    let parts: Vec<&str> = dotted.split('.').collect();
    let (last, prefix) = parts.split_last().expect("keys are never empty");

    let mut cur = doc;
    for part in prefix {
        if !cur.is_table() {
            *cur = Value::Table(Default::default());
        }
        cur = cur
            .as_table_mut()
            .expect("normalized to table")
            .entry(part.to_string())
            .or_insert_with(|| Value::Table(Default::default()));
    }
    if !cur.is_table() {
        *cur = Value::Table(Default::default());
    }
    cur.as_table_mut()
        .expect("normalized to table")
        .insert(last.to_string(), leaf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::ConfigResolver;
    use crate::settings::Settings;
    use valyria_types::{Access, PermissionMode};

    /// Read the written file back the way `config_show` does — layered over
    /// the compiled defaults — since a config file legitimately holds only
    /// the leaves that were set (a partial `[network]` table is normal).
    fn read(path: &Path) -> Settings {
        ConfigResolver::new()
            .workspace_path(path)
            .env_vars(Vec::new())
            .resolve()
            .unwrap()
            .settings
    }

    #[test]
    fn writes_a_new_file_with_one_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("config.toml");
        write_key(&path, "permission.mode", "manual").unwrap();
        assert_eq!(read(&path).permission.mode, PermissionMode::Manual);
    }

    #[test]
    fn preserves_sibling_leaves_on_a_narrow_edit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "[permission]\nmode = \"manual\"\n\n[network]\ninternet = \"controlled\"\n",
        )
        .unwrap();

        write_key(&path, "log.format", "json").unwrap();

        let s = read(&path);
        assert_eq!(s.log.format, crate::settings::LogFormat::Json);
        // untouched siblings survive the round-trip
        assert_eq!(s.permission.mode, PermissionMode::Manual);
        assert_eq!(s.network.internet, Access::Controlled);
    }

    #[test]
    fn rejects_an_unknown_key_without_touching_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let err = write_key(&path, "iteration.limit", "10").unwrap_err();
        assert!(matches!(err, ConfigError::UnknownKey { .. }));
        assert!(!path.exists());
    }

    #[test]
    fn rejects_an_invalid_value_for_a_known_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let err = write_key(&path, "permission.mode", "turbo").unwrap_err();
        assert!(matches!(err, ConfigError::Deserialize { .. }));
        assert!(!path.exists());
    }

    #[test]
    fn policy_floor_rejects_loosening_credentials_and_leaves_the_file_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[permission]\nmode = \"assisted\"\n").unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let err = write_key(&path, "network.credentials", "allowed").unwrap_err();
        assert!(matches!(err, ConfigError::PolicyFloorViolation { .. }));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn a_looser_floor_would_permit_the_same_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let looser = PolicyFloor {
            max_credentials_access: Access::Allowed,
        };
        write_key_with_floor(&path, "network.credentials", "controlled", &looser).unwrap();
        // Read back under the same looser floor (the default floor would,
        // correctly, refuse to resolve this file).
        let settings = ConfigResolver::new()
            .workspace_path(&path)
            .env_vars(Vec::new())
            .floor(looser)
            .resolve()
            .unwrap()
            .settings;
        assert_eq!(settings.network.credentials, Access::Controlled);
    }

    #[test]
    fn no_temp_file_is_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        write_key(&path, "network.internet", "allowed").unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp file not cleaned up: {leftovers:?}"
        );
        assert_eq!(read(&path).network.internet, Access::Allowed);
    }
}
