//! The editing strategy ladder (§19), tried in order of precision. Each
//! variant is a pure `Option<&str> -> Result<String>` transformation —
//! precondition checking, atomic writes, and diffing live in
//! [`crate::transaction`], one layer up.

use crate::error::{EditError, Result};

#[derive(Debug, Clone)]
pub enum EditStrategy {
    /// Strategy 1, exact replacement: `anchor` must appear in the current
    /// content exactly once. Fails loudly (not silently-first-match) on
    /// zero or multiple occurrences.
    ExactReplacement { anchor: String, replacement: String },

    /// Strategies 2 and 3, patch application and unified diff, collapse
    /// to one mechanism here: both are "apply this unified-diff text",
    /// and the underlying `diffy` library already does git-apply-style
    /// fuzzy/offset context matching for both a small single-hunk patch
    /// and a full multi-hunk, multi-region diff.
    UnifiedDiff { diff: String },

    /// Strategy 4, symbol-aware modification — resolving `symbol_path`
    /// requires the repository index (`valyria-index`, Phase 4). Not
    /// implemented yet; see [`EditError::NotYetImplemented`].
    SymbolAware {
        symbol_path: String,
        new_body: String,
    },

    /// Strategy 5, AST transformation — requires a language parser
    /// (`valyria-lang`, Phase 4). Not implemented yet.
    AstTransform { description: String },

    /// Strategy 6, whole-file replacement — permitted only with an
    /// explicit, non-empty `reason`, and gated by a size guard: shrinking
    /// a non-trivial file by more than 90% requires `force: true`.
    WholeFileReplacement {
        content: String,
        reason: String,
        force: bool,
    },
}

/// Shrinking a file smaller than this is never guarded — a 40-byte file
/// becoming a 2-byte file isn't the "oops, replaced the wrong file"
/// scenario the size guard exists to catch.
const SIZE_GUARD_MIN_ORIGINAL_BYTES: usize = 500;
const SIZE_GUARD_SHRINK_THRESHOLD_PCT: u32 = 90;

pub fn apply_strategy(current: Option<&str>, strategy: &EditStrategy) -> Result<String> {
    match strategy {
        EditStrategy::ExactReplacement {
            anchor,
            replacement,
        } => {
            let current = current.ok_or(EditError::NoExistingContent)?;
            let count = current.matches(anchor.as_str()).count();
            match count {
                0 => Err(EditError::AnchorNotFound),
                1 => Ok(current.replacen(anchor.as_str(), replacement, 1)),
                n => Err(EditError::AnchorAmbiguous { count: n }),
            }
        }

        EditStrategy::UnifiedDiff { diff } => {
            let current = current.ok_or(EditError::NoExistingContent)?;
            let patch =
                diffy::Patch::from_str(diff).map_err(|e| EditError::PatchParse(e.to_string()))?;
            diffy::apply(current, &patch).map_err(|e| EditError::PatchApply(e.to_string()))
        }

        EditStrategy::SymbolAware { .. } => Err(EditError::NotYetImplemented {
            strategy: "symbol-aware modification",
            owning_crate: "valyria-index",
        }),

        EditStrategy::AstTransform { .. } => Err(EditError::NotYetImplemented {
            strategy: "AST transformation",
            owning_crate: "valyria-lang",
        }),

        EditStrategy::WholeFileReplacement {
            content,
            reason,
            force,
        } => {
            if reason.trim().is_empty() {
                return Err(EditError::MissingReason);
            }
            if let Some(current) = current {
                if !force && current.len() >= SIZE_GUARD_MIN_ORIGINAL_BYTES {
                    let shrink_pct = shrink_percent(current.len(), content.len());
                    if shrink_pct >= SIZE_GUARD_SHRINK_THRESHOLD_PCT {
                        return Err(EditError::SizeGuardTripped { shrink_pct });
                    }
                }
            }
            Ok(content.clone())
        }
    }
}

fn shrink_percent(before: usize, after: usize) -> u32 {
    if before == 0 {
        return 0;
    }
    if after >= before {
        return 0;
    }
    (((before - after) as f64 / before as f64) * 100.0) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_replacement_replaces_a_unique_anchor() {
        let result = apply_strategy(
            Some("fn old_name() {}"),
            &EditStrategy::ExactReplacement {
                anchor: "old_name".into(),
                replacement: "new_name".into(),
            },
        )
        .unwrap();
        assert_eq!(result, "fn new_name() {}");
    }

    #[test]
    fn exact_replacement_fails_when_anchor_missing() {
        let err = apply_strategy(
            Some("fn foo() {}"),
            &EditStrategy::ExactReplacement {
                anchor: "not_present".into(),
                replacement: "x".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, EditError::AnchorNotFound));
    }

    #[test]
    fn exact_replacement_fails_when_anchor_ambiguous() {
        let err = apply_strategy(
            Some("foo(); foo();"),
            &EditStrategy::ExactReplacement {
                anchor: "foo()".into(),
                replacement: "bar()".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, EditError::AnchorAmbiguous { count: 2 }));
    }

    #[test]
    fn exact_replacement_requires_existing_content() {
        let err = apply_strategy(
            None,
            &EditStrategy::ExactReplacement {
                anchor: "x".into(),
                replacement: "y".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NoExistingContent));
    }

    #[test]
    fn unified_diff_applies_a_single_hunk_patch() {
        let original = "line1\nline2\nline3\n";
        let modified = "line1\nCHANGED\nline3\n";
        let diff = diffy::create_patch(original, modified).to_string();

        let result = apply_strategy(Some(original), &EditStrategy::UnifiedDiff { diff }).unwrap();
        assert_eq!(result, modified);
    }

    #[test]
    fn unified_diff_applies_with_fuzzy_context_offset() {
        // Patch computed against one version of the file; applied to a
        // version with an extra leading line — diffy's context matching
        // should still locate the right hunk.
        let base = "a\nb\nc\n";
        let changed = "a\nB\nc\n";
        let diff = diffy::create_patch(base, changed).to_string();

        let shifted_base = "PREAMBLE\na\nb\nc\n";
        let result =
            apply_strategy(Some(shifted_base), &EditStrategy::UnifiedDiff { diff }).unwrap();
        assert_eq!(result, "PREAMBLE\na\nB\nc\n");
    }

    #[test]
    fn unified_diff_rejects_a_malformed_hunk() {
        // A hunk header declaring 3 old/new lines but a body that doesn't
        // satisfy that count before the input ends — a real, structurally
        // invalid diff, distinct from plain non-diff text (see the test
        // below for that case).
        let malformed = "\
--- a/file.txt
+++ b/file.txt
@@ -1,3 +1,3 @@
-line 1
+LINE 1
garbage before hunk complete
";
        let err = apply_strategy(
            Some("content"),
            &EditStrategy::UnifiedDiff {
                diff: malformed.into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, EditError::PatchParse(_)));
    }

    #[test]
    fn unified_diff_with_no_diff_markers_parses_as_a_no_op() {
        // `diffy` treats text with no `---`/`+++`/`@@` markers as a patch
        // with zero hunks rather than a parse error — applying it is a
        // legitimate no-op at the strategy level. `EditTransaction`
        // (transaction.rs) is what turns "no actual change happened" into
        // a `VerificationFailed` error for the caller.
        let result = apply_strategy(
            Some("content"),
            &EditStrategy::UnifiedDiff {
                diff: "not a diff at all".into(),
            },
        )
        .unwrap();
        assert_eq!(result, "content");
    }

    #[test]
    fn symbol_aware_and_ast_transform_are_not_yet_implemented() {
        let err = apply_strategy(
            Some("x"),
            &EditStrategy::SymbolAware {
                symbol_path: "foo::bar".into(),
                new_body: "{}".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NotYetImplemented { .. }));

        let err = apply_strategy(
            Some("x"),
            &EditStrategy::AstTransform {
                description: "rename".into(),
            },
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NotYetImplemented { .. }));
    }

    #[test]
    fn whole_file_replacement_requires_a_reason() {
        let err = apply_strategy(
            None,
            &EditStrategy::WholeFileReplacement {
                content: "new".into(),
                reason: "".into(),
                force: false,
            },
        )
        .unwrap_err();
        assert!(matches!(err, EditError::MissingReason));
    }

    #[test]
    fn whole_file_replacement_creates_a_new_file() {
        let result = apply_strategy(
            None,
            &EditStrategy::WholeFileReplacement {
                content: "brand new content".into(),
                reason: "creating a new file".into(),
                force: false,
            },
        )
        .unwrap();
        assert_eq!(result, "brand new content");
    }

    #[test]
    fn whole_file_replacement_trips_the_size_guard_on_a_large_shrink() {
        let original = "x".repeat(1000);
        let err = apply_strategy(
            Some(&original),
            &EditStrategy::WholeFileReplacement {
                content: "tiny".into(),
                reason: "oops".into(),
                force: false,
            },
        )
        .unwrap_err();
        assert!(matches!(err, EditError::SizeGuardTripped { shrink_pct } if shrink_pct >= 90));
    }

    #[test]
    fn whole_file_replacement_size_guard_bypassed_with_force() {
        let original = "x".repeat(1000);
        let result = apply_strategy(
            Some(&original),
            &EditStrategy::WholeFileReplacement {
                content: "tiny".into(),
                reason: "intentional rewrite".into(),
                force: true,
            },
        )
        .unwrap();
        assert_eq!(result, "tiny");
    }

    #[test]
    fn whole_file_replacement_size_guard_does_not_trip_on_small_files() {
        // Below SIZE_GUARD_MIN_ORIGINAL_BYTES: shrinking a tiny file
        // entirely is normal, not "oops, wrong file".
        let result = apply_strategy(
            Some("small"),
            &EditStrategy::WholeFileReplacement {
                content: "".into(),
                reason: "clearing it out".into(),
                force: false,
            },
        )
        .unwrap();
        assert_eq!(result, "");
    }

    #[test]
    fn whole_file_replacement_growing_a_file_never_trips_the_guard() {
        let original = "x".repeat(1000);
        let bigger = "y".repeat(2000);
        let result = apply_strategy(
            Some(&original),
            &EditStrategy::WholeFileReplacement {
                content: bigger.clone(),
                reason: "expanding".into(),
                force: false,
            },
        )
        .unwrap();
        assert_eq!(result, bigger);
    }
}
