//! Generic, filesystem-independent path helpers. Deliberately does not
//! touch the filesystem or know about a workspace root — that's
//! `valyria-vfs`'s job (`WorkspacePath::resolve`, layer 1). What lives here
//! is pure string/component logic reusable by anything that needs to
//! sanity-check a path shape before it ever reaches disk.

use std::path::{Component, Path};

/// Whether `path` contains any component that could escape a fixed root if
/// naively joined onto it: `..`, or (on any platform) a component that
/// resolves to an absolute path, which would replace rather than extend
/// the root when joined.
pub fn has_traversal_risk(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

/// Lexically normalize `.` and `..` components without touching the
/// filesystem (no symlink resolution — that requires `vfs`). Returns `None`
/// if normalization would need to climb above the starting point (a `..`
/// with nothing to cancel), which is exactly the traversal case callers
/// need to reject.
pub fn normalize_relative(path: &Path) -> Option<std::path::PathBuf> {
    let mut out: Vec<Component> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                _ => return None,
            },
            Component::Normal(_) => out.push(component),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn flags_parent_dir_traversal() {
        assert!(has_traversal_risk(Path::new("../etc/passwd")));
        assert!(has_traversal_risk(Path::new("src/../../etc/passwd")));
    }

    #[test]
    fn flags_absolute_paths() {
        assert!(has_traversal_risk(Path::new("/etc/passwd")));
    }

    #[test]
    fn allows_ordinary_relative_paths() {
        assert!(!has_traversal_risk(Path::new("src/lib.rs")));
        assert!(!has_traversal_risk(Path::new("./src/lib.rs")));
    }

    #[test]
    fn normalizes_dot_segments() {
        let out = normalize_relative(Path::new("./src/../src/lib.rs")).unwrap();
        assert_eq!(out, Path::new("src/lib.rs"));
    }

    #[test]
    fn rejects_climb_above_root() {
        assert!(normalize_relative(Path::new("../secret")).is_none());
        assert!(normalize_relative(Path::new("src/../../secret")).is_none());
    }

    #[test]
    fn rejects_absolute_input() {
        assert!(normalize_relative(Path::new("/etc/passwd")).is_none());
    }
}
