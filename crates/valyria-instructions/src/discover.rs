//! Walking a workspace for its instruction files.

use std::path::{Path, PathBuf};

use crate::authority::Authority;
use crate::conflict;
use crate::error::{InstructionError, Result};
use crate::source::{
    relativize, FileFingerprint, InstructionFingerprint, InstructionSet, InstructionSource,
};

/// Default size cap for a single instruction file (64 KiB). A file larger
/// than this is read up to the cap at a line boundary and marked
/// truncated — an instruction file that long is almost certainly a
/// generated dump, and letting it consume an unbounded slice of the
/// context budget is worse than truncating it.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024;

/// The instruction filenames looked for at the workspace root, paired
/// with the authority each carries, in the order they are ranked.
const ROOT_DIRECTIVE_FILES: &[(&str, Authority)] = &[
    ("VALYRIA.md", Authority::WorkspaceValyria),
    ("AGENTS.md", Authority::Agents),
    ("CLAUDE.md", Authority::Claude),
];

/// Filenames treated as directory-scoped instruction files when found
/// below the root.
const SCOPED_FILES: &[&str] = &["VALYRIA.md", "AGENTS.md", "CLAUDE.md"];

/// Advisory files: parsed for facts, never obeyed.
const ADVISORY_FILES: &[&str] = &["CONTRIBUTING.md", "README.md", "README"];

#[derive(Debug, Clone)]
pub struct Discovery {
    root: PathBuf,
    user_config: Option<PathBuf>,
    max_bytes: usize,
}

impl Discovery {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            user_config: None,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }

    /// Point discovery at the operator's global instruction file
    /// (`~/.valyria/instructions.md`). A `None` here — or a path that does
    /// not exist — simply yields no [`Authority::UserConfig`] source.
    pub fn with_user_config(mut self, path: Option<PathBuf>) -> Self {
        self.user_config = path;
        self
    }

    pub fn with_max_bytes(mut self, max_bytes: usize) -> Self {
        self.max_bytes = max_bytes.max(1);
        self
    }

    /// Discover every instruction file for the workspace as a whole: the
    /// user config, the root directive files, and the advisory files.
    /// Directory-scoped files are only included by [`Discovery::discover_for_file`].
    pub fn discover(&self) -> Result<InstructionSet> {
        self.discover_inner(&[])
    }

    /// Like [`Discovery::discover`], plus the directory-scoped instruction
    /// files on the path from `edited_rel` (a path relative to the
    /// workspace root) up to — but not including — the root. A file nearer
    /// the edited file outranks one farther from it.
    pub fn discover_for_file(&self, edited_rel: impl AsRef<Path>) -> Result<InstructionSet> {
        let edited_rel = edited_rel.as_ref();
        let mut scoped_dirs: Vec<(PathBuf, usize)> = Vec::new();

        // Ancestors of the edited file's directory, relative to root,
        // shallowest first; depth is the component count.
        let start = edited_rel.parent().unwrap_or(Path::new(""));
        let mut acc = PathBuf::new();
        for (depth, component) in start.components().enumerate() {
            acc.push(component.as_os_str());
            scoped_dirs.push((acc.clone(), depth + 1));
        }

        self.discover_inner(&scoped_dirs)
    }

    fn discover_inner(&self, scoped_dirs: &[(PathBuf, usize)]) -> Result<InstructionSet> {
        let mut sources: Vec<InstructionSource> = Vec::new();
        let mut present: Vec<(PathBuf, FileFingerprint)> = Vec::new();
        let mut absent: Vec<PathBuf> = Vec::new();

        // 1. User config (may live outside the workspace).
        if let Some(path) = &self.user_config {
            self.take(
                path,
                Authority::UserConfig,
                &mut sources,
                &mut present,
                &mut absent,
            )?;
        }

        // 2-4. Root directive files.
        for (name, authority) in ROOT_DIRECTIVE_FILES {
            let path = self.root.join(name);
            self.take(
                &path,
                authority.clone(),
                &mut sources,
                &mut present,
                &mut absent,
            )?;
        }

        // 5. Directory-scoped files (only when discovering for a file).
        for (dir, depth) in scoped_dirs {
            let abs_dir = self.root.join(dir);
            for name in SCOPED_FILES {
                let path = abs_dir.join(name);
                self.take(
                    &path,
                    Authority::DirectoryScoped {
                        dir: dir.clone(),
                        depth: *depth,
                    },
                    &mut sources,
                    &mut present,
                    &mut absent,
                )?;
            }
        }

        // 6. Advisory files.
        for name in ADVISORY_FILES {
            let path = self.root.join(name);
            self.take(
                &path,
                Authority::Advisory,
                &mut sources,
                &mut present,
                &mut absent,
            )?;
        }

        // Stable order: authority rank, then origin path.
        sources.sort_by(|a, b| {
            a.authority
                .rank()
                .cmp(&b.authority.rank())
                .then_with(|| a.origin.cmp(&b.origin))
        });
        present.sort_by(|a, b| a.0.cmp(&b.0));
        absent.sort();

        let conflicts = conflict::detect(&sources);

        Ok(InstructionSet {
            sources,
            conflicts,
            fingerprint: InstructionFingerprint { present, absent },
        })
    }

    /// Read one candidate file. A missing file is recorded in `absent` and
    /// is not an error; a present one becomes a source and is recorded in
    /// `present`.
    fn take(
        &self,
        path: &Path,
        authority: Authority,
        sources: &mut Vec<InstructionSource>,
        present: &mut Vec<(PathBuf, FileFingerprint)>,
        absent: &mut Vec<PathBuf>,
    ) -> Result<()> {
        let key = relativize(path, &self.root);
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                absent.push(key);
                return Ok(());
            }
            Err(source) => {
                return Err(InstructionError::Io {
                    path: path.to_path_buf(),
                    source,
                })
            }
        };
        if !meta.is_file() {
            absent.push(key);
            return Ok(());
        }

        let fingerprint = FileFingerprint::of(&meta);
        let bytes = std::fs::read(path).map_err(|source| InstructionError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let text = String::from_utf8(bytes).map_err(|_| InstructionError::NotUtf8 {
            path: path.to_path_buf(),
        })?;

        let (body, truncated) = cap_at_line_boundary(&text, self.max_bytes);

        present.push((key.clone(), fingerprint));
        sources.push(InstructionSource {
            trust: authority.trust(),
            authority,
            origin: key,
            body,
            truncated,
            bytes_on_disk: meta.len(),
            fingerprint,
        });
        Ok(())
    }
}

/// Truncate `text` to at most `max_bytes` bytes, cutting at the last
/// newline at or before the cap so a directive is never sliced in half.
/// Returns `(body, truncated)`.
fn cap_at_line_boundary(text: &str, max_bytes: usize) -> (String, bool) {
    if text.len() <= max_bytes {
        return (text.to_string(), false);
    }
    // Largest char boundary <= max_bytes.
    let mut cut = max_bytes;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &text[..cut];
    let body = match head.rfind('\n') {
        Some(nl) => &head[..nl + 1],
        None => head,
    };
    (body.to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn an_empty_workspace_yields_an_empty_set() {
        let dir = ws();
        let set = Discovery::new(dir.path()).discover().unwrap();
        assert!(set.is_empty());
        // Every candidate was looked for and recorded absent.
        assert!(set
            .fingerprint
            .absent
            .iter()
            .any(|p| p.ends_with("AGENTS.md")));
        assert!(set.fingerprint.present.is_empty());
    }

    #[test]
    fn root_files_are_found_and_ordered_by_authority() {
        let dir = ws();
        std::fs::write(dir.path().join("CLAUDE.md"), "claude").unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "agents").unwrap();
        std::fs::write(dir.path().join("VALYRIA.md"), "valyria").unwrap();
        std::fs::write(dir.path().join("README.md"), "readme").unwrap();

        let set = Discovery::new(dir.path()).discover().unwrap();
        let authorities: Vec<_> = set.sources.iter().map(|s| s.authority.clone()).collect();
        assert_eq!(
            authorities,
            vec![
                Authority::WorkspaceValyria,
                Authority::Agents,
                Authority::Claude,
                Authority::Advisory,
            ]
        );
    }

    #[test]
    fn advisory_files_get_repo_data_trust() {
        let dir = ws();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        let set = Discovery::new(dir.path()).discover().unwrap();
        assert_eq!(set.sources.len(), 1);
        assert_eq!(set.sources[0].trust, valyria_types::Trust::RepoData);
        assert!(!set.sources[0].is_directive());
    }

    #[test]
    fn user_config_outside_the_workspace_is_included_with_an_absolute_origin() {
        let home = ws();
        let cfg = home.path().join("instructions.md");
        std::fs::write(&cfg, "global rules").unwrap();
        let dir = ws();

        let set = Discovery::new(dir.path())
            .with_user_config(Some(cfg.clone()))
            .discover()
            .unwrap();
        assert_eq!(set.sources.len(), 1);
        assert_eq!(set.sources[0].authority, Authority::UserConfig);
        assert_eq!(set.sources[0].origin, cfg);
    }

    #[test]
    fn oversized_files_truncate_at_a_line_boundary() {
        let dir = ws();
        let body = "line one\nline two\n".repeat(100);
        std::fs::write(dir.path().join("AGENTS.md"), &body).unwrap();

        let set = Discovery::new(dir.path())
            .with_max_bytes(20)
            .discover()
            .unwrap();
        let s = &set.sources[0];
        assert!(s.truncated);
        assert!(s.body.ends_with('\n'));
        assert!(s.body.len() <= 20);
        assert_eq!(s.bytes_on_disk, body.len() as u64);
    }

    #[test]
    fn non_utf8_instruction_file_is_an_error_not_a_lossy_decode() {
        let dir = ws();
        std::fs::write(dir.path().join("CLAUDE.md"), [0xff, 0xfe, 0x00]).unwrap();
        let err = Discovery::new(dir.path()).discover().unwrap_err();
        assert!(matches!(err, InstructionError::NotUtf8 { .. }));
    }

    #[test]
    fn discover_for_file_adds_nearest_first_directory_scoped_sources() {
        let dir = ws();
        std::fs::create_dir_all(dir.path().join("src/parser")).unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "root").unwrap();
        std::fs::write(dir.path().join("src/AGENTS.md"), "src").unwrap();
        std::fs::write(dir.path().join("src/parser/AGENTS.md"), "parser").unwrap();

        let set = Discovery::new(dir.path())
            .discover_for_file("src/parser/grammar.rs")
            .unwrap();

        // Root AGENTS.md first (tier 3), then the two scoped ones with the
        // deeper (parser) one ahead of the shallower (src) one.
        let bodies: Vec<_> = set.sources.iter().map(|s| s.body.as_str()).collect();
        assert_eq!(bodies, vec!["root", "parser", "src"]);
    }

    #[test]
    fn fingerprint_changes_when_a_file_is_edited() {
        let dir = ws();
        std::fs::write(dir.path().join("AGENTS.md"), "v1").unwrap();
        let a = Discovery::new(dir.path()).discover().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "v2 is longer").unwrap();
        let b = Discovery::new(dir.path()).discover().unwrap();
        assert!(a.fingerprint.differs_from(&b.fingerprint));
    }
}
