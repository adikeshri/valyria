//! The edit transaction (§19, D6): precondition check -> apply strategy ->
//! verify the diff -> atomic write -> return an outcome the caller (the
//! change ledger, in `valyria-ledger`) can record.

use std::path::{Path, PathBuf};

use valyria_util::ContentHash;
use valyria_vfs::{HashCache, WorkspaceRoot};

use crate::error::{EditError, Result};
use crate::strategy::{apply_strategy, EditStrategy};

/// What the caller believes the file's current state is, checked before
/// any write — this is the optimistic-concurrency guard from D6. `Any`
/// bypasses the guard and should only be used when the caller has already
/// established freshness some other way (rare; documented as an escape
/// hatch, not the default path).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precondition {
    MustExistWithHash(ContentHash),
    MustNotExist,
    Any,
}

#[derive(Debug, Clone)]
pub struct EditRequest {
    /// Workspace-relative path.
    pub path: PathBuf,
    pub precondition: Precondition,
    pub strategy: EditStrategy,
}

#[derive(Debug, Clone)]
pub struct EditOutcome {
    pub path: PathBuf,
    pub before_hash: Option<ContentHash>,
    pub after_hash: ContentHash,
    /// Unified diff of what actually changed, computed uniformly
    /// regardless of which strategy produced it — this is what a caller
    /// (or a human reviewing the ledger) checks against the intent of the
    /// edit.
    pub diff: String,
    pub before_content: Option<String>,
    pub after_content: String,
}

pub struct EditTransaction<'a> {
    root: &'a WorkspaceRoot,
    hash_cache: &'a HashCache,
}

impl<'a> EditTransaction<'a> {
    pub fn new(root: &'a WorkspaceRoot, hash_cache: &'a HashCache) -> Self {
        Self { root, hash_cache }
    }

    pub fn apply(&self, req: EditRequest) -> Result<EditOutcome> {
        let resolved = self.root.resolve(&req.path)?;

        let current_content = read_if_exists(&resolved)?;
        let current_hash = current_content
            .as_ref()
            .map(|c| ContentHash::of_bytes(c.as_bytes()));

        check_precondition(&req.precondition, current_hash)?;

        let new_content = apply_strategy(current_content.as_deref(), &req.strategy)?;

        // The core of §19's "verify the expected change occurred": if the
        // strategy claims success but produced byte-identical content,
        // something is wrong with the request (e.g. a no-op patch), not a
        // successful edit — surface that rather than silently writing
        // nothing and reporting success.
        if current_content.as_deref() == Some(new_content.as_str()) {
            return Err(EditError::VerificationFailed(
                "strategy produced no change to the file".into(),
            ));
        }

        let diff =
            diffy::create_patch(current_content.as_deref().unwrap_or(""), &new_content).to_string();

        valyria_vfs::write_atomic(&resolved, new_content.as_bytes())?;
        self.hash_cache.invalidate(&resolved);

        let after_hash = ContentHash::of_bytes(new_content.as_bytes());

        Ok(EditOutcome {
            path: req.path,
            before_hash: current_hash,
            after_hash,
            diff,
            before_content: current_content,
            after_content: new_content,
        })
    }
}

fn check_precondition(precondition: &Precondition, actual: Option<ContentHash>) -> Result<()> {
    match precondition {
        Precondition::Any => Ok(()),
        Precondition::MustNotExist => {
            if actual.is_some() {
                Err(EditError::PreconditionFailed {
                    expected: None,
                    actual,
                })
            } else {
                Ok(())
            }
        }
        Precondition::MustExistWithHash(expected) => match actual {
            Some(actual_hash) if actual_hash == *expected => Ok(()),
            _ => Err(EditError::PreconditionFailed {
                expected: Some(*expected),
                actual,
            }),
        },
    }
}

fn read_if_exists(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(EditError::Io {
            path: path.display().to_string(),
            source: e,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (valyria_testkit::TempWorkspace, WorkspaceRoot, HashCache) {
        let ws = valyria_testkit::TempWorkspace::new();
        let root = WorkspaceRoot::new(ws.path()).unwrap();
        let cache = HashCache::new();
        (ws, root, cache)
    }

    #[test]
    fn creates_a_new_file_with_whole_file_replacement() {
        let (ws, root, cache) = setup();
        let tx = EditTransaction::new(&root, &cache);

        let outcome = tx
            .apply(EditRequest {
                path: "new.txt".into(),
                precondition: Precondition::MustNotExist,
                strategy: EditStrategy::WholeFileReplacement {
                    content: "hello".into(),
                    reason: "new file".into(),
                    force: false,
                },
            })
            .unwrap();

        assert_eq!(outcome.before_hash, None);
        assert_eq!(ws.read("new.txt"), "hello");
        assert!(outcome.diff.contains("hello"));
    }

    #[test]
    fn precondition_must_not_exist_fails_if_file_already_there() {
        let (ws, root, cache) = setup();
        ws.write("exists.txt", "already here");
        let tx = EditTransaction::new(&root, &cache);

        let err = tx
            .apply(EditRequest {
                path: "exists.txt".into(),
                precondition: Precondition::MustNotExist,
                strategy: EditStrategy::WholeFileReplacement {
                    content: "overwrite".into(),
                    reason: "test".into(),
                    force: false,
                },
            })
            .unwrap_err();
        assert!(matches!(err, EditError::PreconditionFailed { .. }));
    }

    #[test]
    fn precondition_hash_mismatch_is_rejected_external_modification_protection() {
        let (ws, root, cache) = setup();
        ws.write("f.txt", "original");
        let tx = EditTransaction::new(&root, &cache);

        let stale_hash =
            ContentHash::of_bytes(b"a completely different content the agent thinks is there");
        let err = tx
            .apply(EditRequest {
                path: "f.txt".into(),
                precondition: Precondition::MustExistWithHash(stale_hash),
                strategy: EditStrategy::ExactReplacement {
                    anchor: "original".into(),
                    replacement: "changed".into(),
                },
            })
            .unwrap_err();
        assert!(matches!(err, EditError::PreconditionFailed { .. }));
        // the file on disk must be untouched
        assert_eq!(ws.read("f.txt"), "original");
    }

    #[test]
    fn matching_precondition_allows_the_edit() {
        let (ws, root, cache) = setup();
        ws.write("f.txt", "original");
        let tx = EditTransaction::new(&root, &cache);
        let correct_hash = ContentHash::of_bytes(b"original");

        let outcome = tx
            .apply(EditRequest {
                path: "f.txt".into(),
                precondition: Precondition::MustExistWithHash(correct_hash),
                strategy: EditStrategy::ExactReplacement {
                    anchor: "original".into(),
                    replacement: "changed".into(),
                },
            })
            .unwrap();

        assert_eq!(ws.read("f.txt"), "changed");
        assert_eq!(outcome.before_hash, Some(correct_hash));
        assert_eq!(outcome.after_hash, ContentHash::of_bytes(b"changed"));
    }

    #[test]
    fn a_noop_strategy_is_reported_as_a_verification_failure_not_a_silent_success() {
        let (ws, root, cache) = setup();
        ws.write("f.txt", "content");
        let tx = EditTransaction::new(&root, &cache);

        let err = tx
            .apply(EditRequest {
                path: "f.txt".into(),
                precondition: Precondition::Any,
                strategy: EditStrategy::WholeFileReplacement {
                    content: "content".into(), // identical to current
                    reason: "no-op test".into(),
                    force: false,
                },
            })
            .unwrap_err();
        assert!(matches!(err, EditError::VerificationFailed(_)));
    }

    #[test]
    fn hash_cache_is_invalidated_after_a_successful_write() {
        let (ws, root, cache) = setup();
        ws.write("f.txt", "original");
        // Prime the cache with the old content's hash.
        let resolved = root.resolve("f.txt").unwrap();
        let primed = cache.hash_file(&resolved).unwrap();
        assert_eq!(primed, ContentHash::of_bytes(b"original"));

        let tx = EditTransaction::new(&root, &cache);
        tx.apply(EditRequest {
            path: "f.txt".into(),
            precondition: Precondition::Any,
            strategy: EditStrategy::ExactReplacement {
                anchor: "original".into(),
                replacement: "updated".into(),
            },
        })
        .unwrap();

        let rehashed = cache.hash_file(&resolved).unwrap();
        assert_eq!(rehashed, ContentHash::of_bytes(b"updated"));
    }

    #[test]
    fn diff_reflects_the_actual_change() {
        let (ws, root, cache) = setup();
        ws.write("f.txt", "line1\nline2\nline3\n");
        let tx = EditTransaction::new(&root, &cache);

        let outcome = tx
            .apply(EditRequest {
                path: "f.txt".into(),
                precondition: Precondition::Any,
                strategy: EditStrategy::ExactReplacement {
                    anchor: "line2".into(),
                    replacement: "CHANGED".into(),
                },
            })
            .unwrap();

        assert!(outcome.diff.contains("-line2"));
        assert!(outcome.diff.contains("+CHANGED"));
    }

    #[test]
    fn resolves_paths_through_workspace_root_rejecting_traversal() {
        let (_ws, root, cache) = setup();
        let tx = EditTransaction::new(&root, &cache);

        let err = tx
            .apply(EditRequest {
                path: "../../etc/passwd".into(),
                precondition: Precondition::Any,
                strategy: EditStrategy::WholeFileReplacement {
                    content: "pwned".into(),
                    reason: "malicious".into(),
                    force: false,
                },
            })
            .unwrap_err();
        assert!(matches!(err, EditError::Vfs(_)));
    }
}
