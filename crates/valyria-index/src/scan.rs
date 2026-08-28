//! Reading the workspace: the parallel, side-effect-free half of
//! indexing.
//!
//! Scanning produces plain values and touches no database. That split is
//! what lets the expensive work (hashing, parsing, extraction) run across
//! every core on `rayon` while the single-writer store actor (D7) sees
//! exactly one transaction at the end.

use std::path::{Path, PathBuf};

use rayon::prelude::*;
use valyria_lang::{FileFacts, LanguageRegistry};
use valyria_util::ContentHash;

use crate::record::{FileRecord, RelPath};

/// Scanning limits. Defaults match §4.4's caps; a caller that knows its
/// repository can widen them.
#[derive(Debug, Clone, Copy)]
pub struct ScanOptions {
    /// Files larger than this are recorded (so search can still find them
    /// by path) but never parsed. A 40MB generated file has nothing the
    /// symbol index wants and would stall the bootstrap.
    pub max_parse_bytes: u64,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_parse_bytes: valyria_vfs::DEFAULT_MAX_CONTEXT_FILE_BYTES,
        }
    }
}

/// One file, fully scanned.
#[derive(Debug, Clone)]
pub struct ScannedFile {
    pub record: FileRecord,
    pub facts: FileFacts,
}

/// Progress during a bootstrap, reported through a caller-supplied
/// callback rather than an event bus: the index is layer 2 and should not
/// decide how a layer-6 client wants to be told (§4.2 events are
/// projections owned further up).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanProgress {
    /// The walk finished; `files` is how many will be processed.
    Discovered { files: usize },
    /// A batch of files finished scanning.
    Scanned { done: usize, total: usize },
}

/// Walk the workspace, honoring `.gitignore`, and return one
/// [`ScannedFile`] per file in a deterministic (path-sorted) order.
///
/// Unreadable files are skipped with a warning rather than failing the
/// whole bootstrap: a repository containing one file with hostile
/// permissions must still index.
pub fn scan_workspace(
    root: &Path,
    registry: &LanguageRegistry,
    options: ScanOptions,
    progress: &(dyn Fn(ScanProgress) + Sync),
) -> Vec<ScannedFile> {
    let absolute = match valyria_vfs::list_files(root) {
        Ok(files) => files,
        Err(e) => {
            tracing::warn!(root = %root.display(), error = %e, "workspace walk failed");
            return Vec::new();
        }
    };

    let candidates: Vec<(PathBuf, RelPath)> = absolute
        .into_iter()
        .filter_map(|abs| {
            let rel = relative_path(root, &abs)?;
            // The runtime's own state directory is not repository content.
            // Indexing it would put the task journal into search results
            // and make every write to it look like a source change.
            if rel == ".valyria" || rel.starts_with(".valyria/") {
                return None;
            }
            Some((abs, rel))
        })
        .collect();

    progress(ScanProgress::Discovered {
        files: candidates.len(),
    });

    let total = candidates.len();
    let done = std::sync::atomic::AtomicUsize::new(0);

    let mut scanned: Vec<ScannedFile> = candidates
        .par_iter()
        .filter_map(|(abs, rel)| {
            let result = scan_one(abs, rel, registry, options);
            let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            // Report every 256 files rather than every file: at 100k files
            // the callback itself would otherwise become measurable.
            if n.is_multiple_of(256) || n == total {
                progress(ScanProgress::Scanned { done: n, total });
            }
            match result {
                Ok(file) => Some(file),
                Err(e) => {
                    tracing::warn!(path = %rel, error = %e, "skipping unreadable file");
                    None
                }
            }
        })
        .collect();

    // Parallel collection order is not guaranteed to be stable across
    // runs; the drift check compares two scans directly, so sort.
    scanned.sort_by(|a, b| a.record.path.cmp(&b.record.path));
    scanned
}

/// Scan exactly these paths (workspace-relative). Used by the incremental
/// pipeline, where re-walking the whole tree to learn about three changed
/// files would defeat the point.
pub fn scan_paths(
    root: &Path,
    paths: &[RelPath],
    registry: &LanguageRegistry,
    options: ScanOptions,
) -> Vec<ScannedFile> {
    let mut scanned: Vec<ScannedFile> = paths
        .par_iter()
        .filter_map(|rel| {
            let abs = root.join(rel);
            scan_one(&abs, rel, registry, options).ok()
        })
        .collect();
    scanned.sort_by(|a, b| a.record.path.cmp(&b.record.path));
    scanned
}

fn scan_one(
    abs: &Path,
    rel: &str,
    registry: &LanguageRegistry,
    options: ScanOptions,
) -> std::io::Result<ScannedFile> {
    let bytes = std::fs::read(abs)?;
    let size_bytes = bytes.len() as u64;
    let content_hash = ContentHash::of_bytes(&bytes);
    let is_binary = valyria_vfs::looks_binary(&bytes);

    let language = registry.language_id_for_path(Path::new(rel));

    // Three reasons not to parse, each of which still leaves a usable file
    // record behind: no grammar, binary content, or too large to be worth
    // it. `text` is `None` in all three.
    let text = if is_binary || size_bytes > options.max_parse_bytes {
        None
    } else {
        String::from_utf8(bytes).ok()
    };

    let line_count = text
        .as_deref()
        .map(|t| t.lines().count() as u32)
        .unwrap_or(0);

    let facts = match (language, text.as_deref()) {
        (Some(id), Some(source)) => registry
            .extract_facts_as(id, source)
            .unwrap_or_else(|e| {
                tracing::warn!(path = %rel, error = %e, "extraction failed; indexing file without symbols");
                FileFacts::default()
            }),
        _ => FileFacts::default(),
    };

    Ok(ScannedFile {
        record: FileRecord {
            path: rel.to_string(),
            language: language.map(|s| s.to_string()),
            content_hash,
            size_bytes,
            line_count,
            is_binary,
            has_parse_errors: facts.has_parse_errors,
        },
        facts,
    })
}

/// Workspace-relative, `/`-separated. Returns `None` for a path outside
/// `root`, which the walk should never produce but which a caller-supplied
/// path might.
pub fn relative_path(root: &Path, absolute: &Path) -> Option<RelPath> {
    let rel = absolute.strip_prefix(root).ok()?;
    let mut parts = Vec::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            std::path::Component::CurDir => {}
            // `..`, a root, or a prefix means the path is not cleanly
            // inside the workspace; the VFS would reject it too.
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> LanguageRegistry {
        LanguageRegistry::with_builtin_languages().unwrap()
    }

    #[test]
    fn scans_a_workspace_and_extracts_symbols() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("src/lib.rs", "pub fn alpha() {}\npub fn beta() {}\n")
            .write("README.md", "# hi\n");

        let scanned = scan_workspace(ws.path(), &registry(), ScanOptions::default(), &|_| {});
        let paths: Vec<&str> = scanned.iter().map(|s| s.record.path.as_str()).collect();
        assert_eq!(paths, ["README.md", "src/lib.rs"]);

        let lib = scanned
            .iter()
            .find(|s| s.record.path == "src/lib.rs")
            .unwrap();
        assert_eq!(lib.record.language.as_deref(), Some("rust"));
        assert_eq!(lib.facts.symbols.len(), 2);
    }

    #[test]
    fn a_file_with_no_grammar_is_still_indexed_without_symbols() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("notes.md", "# Design notes\n");

        let scanned = scan_workspace(ws.path(), &registry(), ScanOptions::default(), &|_| {});
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].record.language, None);
        assert!(scanned[0].facts.symbols.is_empty());
        assert!(!scanned[0].record.is_binary);
    }

    #[test]
    fn binary_files_are_recorded_but_never_parsed() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("blob.rs", [0u8, 1, 2, 3, 0, 255]);

        let scanned = scan_workspace(ws.path(), &registry(), ScanOptions::default(), &|_| {});
        assert_eq!(scanned.len(), 1);
        assert!(scanned[0].record.is_binary);
        assert!(scanned[0].facts.symbols.is_empty());
        assert_eq!(scanned[0].record.line_count, 0);
    }

    #[test]
    fn oversized_files_are_recorded_but_never_parsed() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("big.rs", "pub fn f() {}\n".repeat(100));

        let options = ScanOptions {
            max_parse_bytes: 10,
        };
        let scanned = scan_workspace(ws.path(), &registry(), options, &|_| {});
        assert_eq!(scanned.len(), 1);
        assert!(scanned[0].facts.symbols.is_empty());
        assert!(scanned[0].record.size_bytes > 10);
    }

    #[test]
    fn gitignored_files_never_reach_the_index() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write(".gitignore", "target/\n")
            .write("src/lib.rs", "pub fn f() {}")
            .write("target/debug/build.rs", "pub fn generated() {}");

        let scanned = scan_workspace(ws.path(), &registry(), ScanOptions::default(), &|_| {});
        let paths: Vec<&str> = scanned.iter().map(|s| s.record.path.as_str()).collect();
        assert!(!paths.iter().any(|p| p.starts_with("target/")));
        assert!(paths.contains(&"src/lib.rs"));
    }

    #[test]
    fn the_runtimes_own_state_directory_is_not_repository_content() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("src/lib.rs", "pub fn f() {}")
            .write(".valyria/workspace.db", "not really a db")
            .write(".valyria/cache/x.json", "{}");

        let scanned = scan_workspace(ws.path(), &registry(), ScanOptions::default(), &|_| {});
        let paths: Vec<&str> = scanned.iter().map(|s| s.record.path.as_str()).collect();
        assert_eq!(paths, ["src/lib.rs"]);
    }

    #[test]
    fn progress_reports_discovery_and_completion() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("a.rs", "pub fn a() {}")
            .write("b.rs", "pub fn b() {}");

        let seen = std::sync::Mutex::new(Vec::new());
        scan_workspace(ws.path(), &registry(), ScanOptions::default(), &|p| {
            seen.lock().unwrap().push(p);
        });

        let seen = seen.into_inner().unwrap();
        assert_eq!(seen[0], ScanProgress::Discovered { files: 2 });
        assert_eq!(
            seen.last(),
            Some(&ScanProgress::Scanned { done: 2, total: 2 })
        );
    }

    #[test]
    fn scan_paths_reads_only_what_it_was_asked_for() {
        let ws = valyria_testkit::TempWorkspace::new();
        ws.write("a.rs", "pub fn a() {}")
            .write("b.rs", "pub fn b() {}");

        let scanned = scan_paths(
            ws.path(),
            &["b.rs".to_string()],
            &registry(),
            ScanOptions::default(),
        );
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].record.path, "b.rs");
    }

    #[test]
    fn scan_paths_silently_skips_a_path_that_no_longer_exists() {
        // The incremental pipeline hands over paths a watcher reported;
        // by the time scanning runs the file may already be gone again.
        let ws = valyria_testkit::TempWorkspace::new();
        let scanned = scan_paths(
            ws.path(),
            &["vanished.rs".to_string()],
            &registry(),
            ScanOptions::default(),
        );
        assert!(scanned.is_empty());
    }

    #[test]
    fn relative_paths_are_slash_separated_and_reject_escapes() {
        let root = Path::new("/repo");
        assert_eq!(
            relative_path(root, Path::new("/repo/src/lib.rs")).as_deref(),
            Some("src/lib.rs")
        );
        assert_eq!(relative_path(root, Path::new("/elsewhere/x.rs")), None);
    }

    #[test]
    fn scanning_is_deterministic_across_runs() {
        let ws = valyria_testkit::TempWorkspace::new();
        for i in 0..20 {
            ws.write(format!("src/m{i}.rs"), format!("pub fn f{i}() {{}}\n"));
        }

        let a = scan_workspace(ws.path(), &registry(), ScanOptions::default(), &|_| {});
        let b = scan_workspace(ws.path(), &registry(), ScanOptions::default(), &|_| {});
        let a_paths: Vec<&str> = a.iter().map(|s| s.record.path.as_str()).collect();
        let b_paths: Vec<&str> = b.iter().map(|s| s.record.path.as_str()).collect();
        assert_eq!(a_paths, b_paths);
    }
}
