//! Golden-file testing: assert actual output against a checked-in fixture,
//! with an update mode for regenerating fixtures deliberately rather than
//! hand-editing them.
//!
//! Callers pass their own crate's manifest-relative golden directory
//! (typically `env!("CARGO_MANIFEST_DIR")` joined with e.g. `"testdata"`)
//! since `CARGO_MANIFEST_DIR` at compile time always refers to whatever
//! crate is being compiled — resolving it inside `valyria-testkit` itself
//! would point at this crate, not the caller's.

use std::path::Path;

/// Compare `actual` against the golden file at `golden_dir/name`.
///
/// Set `VALYRIA_UPDATE_GOLDEN=1` in the environment to write `actual` as
/// the new golden instead of comparing — the standard "regenerate fixtures
/// on purpose" escape hatch.
pub fn assert_golden(golden_dir: &Path, name: &str, actual: &str) {
    let update = std::env::var_os("VALYRIA_UPDATE_GOLDEN").is_some();
    assert_golden_with_mode(golden_dir, name, actual, update);
}

/// The mode-explicit core, factored out so tests can exercise both branches
/// without mutating the process-global environment (which would race
/// against every other test in the same binary — `cargo test` runs tests
/// in parallel by default).
pub fn assert_golden_with_mode(golden_dir: &Path, name: &str, actual: &str, update: bool) {
    let path = golden_dir.join(name);

    if update {
        std::fs::create_dir_all(golden_dir).unwrap_or_else(|e| {
            panic!("failed to create golden dir {}: {e}", golden_dir.display())
        });
        std::fs::write(&path, actual)
            .unwrap_or_else(|e| panic!("failed to write golden file {}: {e}", path.display()));
        return;
    }

    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "golden file missing: {} (run with VALYRIA_UPDATE_GOLDEN=1 to create it)",
            path.display()
        )
    });

    assert_eq!(actual, expected, "golden mismatch for {}", path.display());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_an_existing_golden() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("case.txt"), "expected output").unwrap();
        assert_golden_with_mode(dir.path(), "case.txt", "expected output", false);
    }

    #[test]
    #[should_panic(expected = "golden mismatch")]
    fn panics_on_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("case.txt"), "expected").unwrap();
        assert_golden_with_mode(dir.path(), "case.txt", "different", false);
    }

    #[test]
    #[should_panic(expected = "golden file missing")]
    fn panics_when_golden_is_absent_and_not_updating() {
        let dir = tempfile::tempdir().unwrap();
        assert_golden_with_mode(dir.path(), "never-created.txt", "anything", false);
    }

    #[test]
    fn update_mode_writes_the_golden_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_golden_with_mode(dir.path(), "new.txt", "freshly generated", true);

        assert_eq!(
            std::fs::read_to_string(dir.path().join("new.txt")).unwrap(),
            "freshly generated"
        );
    }

    #[test]
    fn update_mode_then_comparison_mode_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        assert_golden_with_mode(dir.path(), "rt.txt", "content v1", true);
        assert_golden_with_mode(dir.path(), "rt.txt", "content v1", false);
    }
}
