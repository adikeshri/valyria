//! The edit transaction (§19, D6): precondition check -> apply strategy ->
//! verify the diff -> atomic write -> return an outcome the caller (the
//! change ledger, in `valyria-ledger`) can record.

use std::path::{Path, PathBuf};

use valyria_lang::LanguageRegistry;
use valyria_util::ContentHash;
use valyria_vfs::{HashCache, WorkspaceRoot};

use crate::error::{EditError, Result};
use crate::strategy::{apply_strategy, EditContext, EditStrategy};

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
    languages: Option<&'a LanguageRegistry>,
}

impl<'a> EditTransaction<'a> {
    pub fn new(root: &'a WorkspaceRoot, hash_cache: &'a HashCache) -> Self {
        Self {
            root,
            hash_cache,
            languages: None,
        }
    }

    /// Attach the language registry.
    ///
    /// Two things become possible: the symbol-aware and AST strategies
    /// run at all, and *every* strategy gains the §4.11 re-parse guard —
    /// a file that parsed before an edit must still parse after it.
    /// Without a registry the transaction still works; it just cannot
    /// offer either.
    pub fn with_languages(mut self, languages: &'a LanguageRegistry) -> Self {
        self.languages = Some(languages);
        self
    }

    pub fn apply(&self, req: EditRequest) -> Result<EditOutcome> {
        let resolved = self.root.resolve(&req.path)?;

        let current_content = read_if_exists(&resolved)?;
        let current_hash = current_content
            .as_ref()
            .map(|c| ContentHash::of_bytes(c.as_bytes()));

        check_precondition(&req.precondition, current_hash)?;

        let ctx = EditContext {
            path: Some(&req.path),
            languages: self.languages,
        };
        let new_content = apply_strategy(current_content.as_deref(), &req.strategy, &ctx)?;

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

        // §4.11's second verification: "if the file parsed before it must
        // parse after". Checked here rather than inside each strategy so
        // an exact replacement or a unified diff is held to it too — those
        // are just as capable of deleting a closing brace, and are used
        // far more often than the AST strategies.
        self.check_no_syntax_regression(&req.path, current_content.as_deref(), &new_content)?;

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

impl EditTransaction<'_> {
    /// Reject an edit that introduces syntax errors into a file that had
    /// none.
    ///
    /// Deliberately one-directional: a file that already failed to parse
    /// is not held to the standard, because the agent is often midway
    /// through fixing exactly that, and refusing the repair would be the
    /// worst possible moment to be strict.
    fn check_no_syntax_regression(
        &self,
        path: &Path,
        before: Option<&str>,
        after: &str,
    ) -> Result<()> {
        let Some(registry) = self.languages else {
            return Ok(());
        };
        let Some(lang) = registry.for_path(path) else {
            return Ok(());
        };
        let Some(before) = before else {
            return Ok(()); // a new file has no prior state to regress from
        };

        if lang.parse(before).map(|p| p.has_errors()).unwrap_or(true) {
            return Ok(());
        }
        if lang.parse(after).map(|p| p.has_errors()).unwrap_or(false) {
            return Err(EditError::SyntaxRegression);
        }
        Ok(())
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

    fn languages() -> &'static LanguageRegistry {
        static REGISTRY: std::sync::OnceLock<LanguageRegistry> = std::sync::OnceLock::new();
        REGISTRY.get_or_init(|| LanguageRegistry::with_builtin_languages().unwrap())
    }

    #[test]
    fn an_edit_that_breaks_a_file_that_parsed_is_refused_and_nothing_is_written() {
        // §4.11: "if the file parsed before it must parse after". Applied
        // to an exact replacement, not just the AST strategies — those are
        // used far more often and are just as capable of deleting a brace.
        let (ws, root, cache) = setup();
        ws.write("src/lib.rs", "pub fn good() {\n    let x = 1;\n}\n");
        let tx = EditTransaction::new(&root, &cache).with_languages(languages());

        let err = tx
            .apply(EditRequest {
                path: "src/lib.rs".into(),
                precondition: Precondition::Any,
                strategy: EditStrategy::ExactReplacement {
                    anchor: "let x = 1;\n}".into(),
                    replacement: "let x = 1;".into(), // eats the closing brace
                },
            })
            .unwrap_err();

        assert!(matches!(err, EditError::SyntaxRegression));
        assert_eq!(
            ws.read("src/lib.rs"),
            "pub fn good() {\n    let x = 1;\n}\n"
        );
    }

    #[test]
    fn an_edit_that_repairs_an_already_broken_file_is_allowed() {
        // The guard is one-directional on purpose: refusing to touch a
        // file that already fails to parse would block the agent at
        // exactly the moment it is fixing that.
        let (ws, root, cache) = setup();
        ws.write("src/lib.rs", "pub fn broken( {\n");
        let tx = EditTransaction::new(&root, &cache).with_languages(languages());

        tx.apply(EditRequest {
            path: "src/lib.rs".into(),
            precondition: Precondition::Any,
            strategy: EditStrategy::WholeFileReplacement {
                content: "pub fn fixed() {}\n".into(),
                reason: "repair the syntax error".into(),
                force: false,
            },
        })
        .unwrap();

        assert_eq!(ws.read("src/lib.rs"), "pub fn fixed() {}\n");
    }

    #[test]
    fn a_file_with_no_grammar_is_edited_without_a_syntax_check() {
        let (ws, root, cache) = setup();
        ws.write("notes.md", "# Title\n");
        let tx = EditTransaction::new(&root, &cache).with_languages(languages());

        tx.apply(EditRequest {
            path: "notes.md".into(),
            precondition: Precondition::Any,
            strategy: EditStrategy::ExactReplacement {
                anchor: "Title".into(),
                replacement: "Heading".into(),
            },
        })
        .unwrap();

        assert_eq!(ws.read("notes.md"), "# Heading\n");
    }

    #[test]
    fn a_symbol_aware_edit_runs_end_to_end_through_the_transaction() {
        let (ws, root, cache) = setup();
        ws.write(
            "src/lib.rs",
            "pub fn keep() {}\n\npub fn target() -> u32 {\n    0\n}\n",
        );
        let tx = EditTransaction::new(&root, &cache).with_languages(languages());

        let outcome = tx
            .apply(EditRequest {
                path: "src/lib.rs".into(),
                precondition: Precondition::Any,
                strategy: EditStrategy::SymbolAware {
                    symbol_path: "target".into(),
                    replacement: "pub fn target() -> u32 {\n    42\n}".into(),
                },
            })
            .unwrap();

        assert!(ws.read("src/lib.rs").contains("42"));
        assert!(ws.read("src/lib.rs").contains("pub fn keep() {}"));
        assert!(outcome.diff.contains("+    42"), "{}", outcome.diff);
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
