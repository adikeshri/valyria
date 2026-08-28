//! The editing strategy ladder (§19), tried in order of precision. Each
//! variant is a pure `Option<&str> -> Result<String>` transformation —
//! precondition checking, atomic writes, and diffing live in
//! [`crate::transaction`], one layer up.

use std::path::Path;

use valyria_lang::{CompiledLanguage, LanguageRegistry};

use crate::ast::{self, AstTransform};
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

    /// Strategy 4, symbol-aware modification: replace the definition
    /// named `symbol_path` (`Parser::parse`, `Outer.Inner.method`).
    ///
    /// The symbol is resolved against the file's *current* content rather
    /// than against the index. The index is what tells a caller which file
    /// to edit; within that file the bytes on disk are the only thing an
    /// edit can safely be positioned against, and a generation-old index
    /// would put the span in the wrong place. `replacement` covers the
    /// whole definition — including its signature, so a signature change
    /// is expressible — and its continuation lines are re-indented to the
    /// definition's own column.
    SymbolAware {
        symbol_path: String,
        replacement: String,
    },

    /// Strategy 5, AST transformation: a typed operation resolved through
    /// the syntax tree and validated by a re-parse. See [`AstTransform`].
    Ast { transform: AstTransform },

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

/// What the parser-backed strategies need: the file being edited, and the
/// languages this build knows.
///
/// Held by reference rather than owned because compiling every grammar and
/// query costs milliseconds; one registry is built per runtime, not per
/// edit.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditContext<'a> {
    pub path: Option<&'a Path>,
    pub languages: Option<&'a LanguageRegistry>,
}

impl<'a> EditContext<'a> {
    pub fn new(path: &'a Path, languages: &'a LanguageRegistry) -> Self {
        Self {
            path: Some(path),
            languages: Some(languages),
        }
    }

    /// The grammar for the file under edit.
    ///
    /// Absent in two situations that look the same to a caller and are
    /// therefore reported the same way: this build has no grammar for the
    /// file type, or the transaction was constructed without a registry.
    /// Either way the parser-based strategies cannot run, and saying so is
    /// better than falling back to a text-based approximation of them.
    pub fn language(&self) -> Result<&'a CompiledLanguage> {
        let path = self.path.unwrap_or_else(|| Path::new("<unknown>"));
        self.languages
            .and_then(|registry| registry.for_path(path))
            .map(|lang| lang.as_ref())
            .ok_or_else(|| EditError::LanguageUnavailable {
                path: path.display().to_string(),
            })
    }
}

/// The indentation of the line `byte` sits on.
fn leading_indent(source: &str, byte: usize) -> String {
    let line_start = source[..byte.min(source.len())]
        .rfind('\n')
        .map(|i| i + 1)
        .unwrap_or(0);
    source[line_start..byte.min(source.len())]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect()
}

/// Indent every line of `text` after the first. The first line is spliced
/// in where the old definition started, which is already at the right
/// column; the rest would otherwise land at column zero.
fn reindent_continuation_lines(text: &str, indent: &str) -> String {
    let mut lines = text.lines();
    let first = lines.next().unwrap_or("").to_string();
    let rest: Vec<String> = lines
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("{indent}{line}")
            }
        })
        .collect();
    if rest.is_empty() {
        first
    } else {
        format!("{first}\n{}", rest.join("\n"))
    }
}

pub fn apply_strategy(
    current: Option<&str>,
    strategy: &EditStrategy,
    ctx: &EditContext<'_>,
) -> Result<String> {
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

        EditStrategy::SymbolAware {
            symbol_path,
            replacement,
        } => {
            let current = current.ok_or(EditError::NoExistingContent)?;
            let lang = ctx.language()?;
            let span = ast::resolve_symbol_span(lang, current, symbol_path)?;
            let indent = leading_indent(current, span.start_byte);
            let body = reindent_continuation_lines(replacement, &indent);

            let mut out = String::with_capacity(current.len() + body.len());
            out.push_str(&current[..span.start_byte]);
            out.push_str(&body);
            out.push_str(&current[span.end_byte.min(current.len())..]);
            Ok(out)
        }

        EditStrategy::Ast { transform } => {
            let current = current.ok_or(EditError::NoExistingContent)?;
            ast::apply(ctx.language()?, current, transform)
        }

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

    /// A context with no registry: what a caller that never attached one
    /// gets, and all the text-based strategies need.
    fn plain() -> EditContext<'static> {
        EditContext::default()
    }

    fn rust_registry() -> &'static LanguageRegistry {
        // Compiling every grammar and query costs milliseconds, so the
        // test module builds one registry and shares it, exactly as the
        // runtime does.
        static REGISTRY: std::sync::OnceLock<LanguageRegistry> = std::sync::OnceLock::new();
        REGISTRY.get_or_init(|| LanguageRegistry::with_builtin_languages().unwrap())
    }

    fn rust_context() -> EditContext<'static> {
        EditContext::new(Path::new("src/lib.rs"), rust_registry())
    }

    #[test]
    fn exact_replacement_replaces_a_unique_anchor() {
        let result = apply_strategy(
            Some("fn old_name() {}"),
            &EditStrategy::ExactReplacement {
                anchor: "old_name".into(),
                replacement: "new_name".into(),
            },
            &plain(),
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
            &plain(),
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
            &plain(),
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
            &plain(),
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NoExistingContent));
    }

    #[test]
    fn unified_diff_applies_a_single_hunk_patch() {
        let original = "line1\nline2\nline3\n";
        let modified = "line1\nCHANGED\nline3\n";
        let diff = diffy::create_patch(original, modified).to_string();

        let result = apply_strategy(
            Some(original),
            &EditStrategy::UnifiedDiff { diff },
            &plain(),
        )
        .unwrap();
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
        let result = apply_strategy(
            Some(shifted_base),
            &EditStrategy::UnifiedDiff { diff },
            &plain(),
        )
        .unwrap();
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
            &plain(),
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
            &plain(),
        )
        .unwrap();
        assert_eq!(result, "content");
    }

    #[test]
    fn the_parser_backed_strategies_report_a_missing_grammar_rather_than_guessing() {
        // Without a registry there is no honest way to find a symbol, and
        // falling back to text search would silently make a different edit
        // than the one that was asked for.
        let err = apply_strategy(
            Some("fn foo() {}"),
            &EditStrategy::SymbolAware {
                symbol_path: "foo".into(),
                replacement: "fn foo() { 1 }".into(),
            },
            &plain(),
        )
        .unwrap_err();
        assert!(matches!(err, EditError::LanguageUnavailable { .. }));
    }

    #[test]
    fn symbol_aware_replaces_the_whole_definition() {
        let source = "fn keep() {}\n\nfn target() {\n    old();\n}\n\nfn also_keep() {}\n";
        let result = apply_strategy(
            Some(source),
            &EditStrategy::SymbolAware {
                symbol_path: "target".into(),
                replacement: "fn target(x: u32) -> u32 {\n    x + 1\n}".into(),
            },
            &rust_context(),
        )
        .unwrap();

        assert!(result.contains("fn target(x: u32) -> u32 {"));
        assert!(!result.contains("old();"));
        // Its neighbours are untouched.
        assert!(result.contains("fn keep() {}"));
        assert!(result.contains("fn also_keep() {}"));
    }

    #[test]
    fn symbol_aware_addresses_a_method_by_its_qualified_path() {
        let source = "struct P;\nimpl P {\n    fn parse(&self) -> bool { false }\n}\n";
        let result = apply_strategy(
            Some(source),
            &EditStrategy::SymbolAware {
                symbol_path: "P::parse".into(),
                replacement: "fn parse(&self) -> bool { true }".into(),
            },
            &rust_context(),
        )
        .unwrap();
        assert!(result.contains("fn parse(&self) -> bool { true }"));
    }

    #[test]
    fn symbol_aware_reindents_the_replacements_continuation_lines() {
        let source = "impl P {\n    fn parse(&self) -> bool { false }\n}\n";
        let result = apply_strategy(
            Some(source),
            &EditStrategy::SymbolAware {
                symbol_path: "P::parse".into(),
                replacement: "fn parse(&self) -> bool {\n    true\n}".into(),
            },
            &rust_context(),
        )
        .unwrap();

        // The replacement was written at column zero; it lands at the
        // method's own indentation.
        assert!(
            result.contains("    fn parse(&self) -> bool {\n        true\n    }"),
            "{result}"
        );
    }

    #[test]
    fn symbol_aware_names_the_available_symbols_when_the_target_is_missing() {
        let err = apply_strategy(
            Some("fn alpha() {}\nfn beta() {}\n"),
            &EditStrategy::SymbolAware {
                symbol_path: "gamma".into(),
                replacement: "fn gamma() {}".into(),
            },
            &rust_context(),
        )
        .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("alpha"), "{message}");
        assert!(message.contains("beta"), "{message}");
    }

    #[test]
    fn symbol_aware_refuses_an_ambiguous_target_rather_than_picking_one() {
        // Two `impl` blocks for the same type — routine in real Rust, one
        // per `cfg` — give two definitions of `P::helper`. Neither reading
        // of the request is more correct than the other, so writing either
        // one to disk would be a coin flip.
        let source =
            "struct P;\nimpl P {\n    fn helper() {}\n}\nimpl P {\n    fn helper() {}\n}\n";
        let err = apply_strategy(
            Some(source),
            &EditStrategy::SymbolAware {
                symbol_path: "P::helper".into(),
                replacement: "fn helper() { 1 }".into(),
            },
            &rust_context(),
        )
        .unwrap_err();
        assert!(
            matches!(err, EditError::SymbolAmbiguous { count: 2, .. }),
            "{err}"
        );
    }

    #[test]
    fn ast_rename_skips_strings_and_comments() {
        let source =
            "// helper does things\nfn helper() {}\nfn run() { let s = \"helper\"; helper(); }\n";
        let result = apply_strategy(
            Some(source),
            &EditStrategy::Ast {
                transform: AstTransform::RenameIdentifier {
                    from: "helper".into(),
                    to: "assist".into(),
                },
            },
            &rust_context(),
        )
        .unwrap();

        assert!(result.contains("fn assist() {}"));
        assert!(result.contains("assist();"));
        // The comment and the string literal are left exactly as they were.
        assert!(result.contains("// helper does things"), "{result}");
        assert!(result.contains("\"helper\""), "{result}");
    }

    #[test]
    fn ast_rename_of_an_absent_identifier_is_an_error_not_a_silent_no_op() {
        let err = apply_strategy(
            Some("fn a() {}"),
            &EditStrategy::Ast {
                transform: AstTransform::RenameIdentifier {
                    from: "nowhere".into(),
                    to: "somewhere".into(),
                },
            },
            &rust_context(),
        )
        .unwrap_err();
        assert!(matches!(err, EditError::NoMatch(_)));
    }

    #[test]
    fn ast_delete_symbol_removes_the_whole_line() {
        let source = "fn keep() {}\nfn gone() {}\nfn also_keep() {}\n";
        let result = apply_strategy(
            Some(source),
            &EditStrategy::Ast {
                transform: AstTransform::DeleteSymbol {
                    symbol_path: "gone".into(),
                },
            },
            &rust_context(),
        )
        .unwrap();
        assert_eq!(result, "fn keep() {}\nfn also_keep() {}\n");
    }

    #[test]
    fn ast_insert_before_symbol_places_text_at_the_symbols_indentation() {
        let source = "impl P {\n    fn parse(&self) {}\n}\n";
        let result = apply_strategy(
            Some(source),
            &EditStrategy::Ast {
                transform: AstTransform::InsertBeforeSymbol {
                    symbol_path: "P::parse".into(),
                    text: "/// Parses.".into(),
                },
            },
            &rust_context(),
        )
        .unwrap();
        assert_eq!(
            result,
            "impl P {\n    /// Parses.\n    fn parse(&self) {}\n}\n"
        );
    }

    #[test]
    fn ast_query_replacement_refuses_several_matches_unless_asked_for_all() {
        let source = "fn alpha() {}\nfn beta() {}\n";
        let transform = |all| EditStrategy::Ast {
            transform: AstTransform::ReplaceQueryMatch {
                query: "(function_item name: (identifier) @name)".into(),
                capture: "name".into(),
                replacement: "renamed".into(),
                all,
            },
        };

        let err = apply_strategy(Some(source), &transform(false), &rust_context()).unwrap_err();
        assert!(matches!(err, EditError::QueryAmbiguous { count: 2 }));

        let result = apply_strategy(Some(source), &transform(true), &rust_context()).unwrap();
        assert_eq!(result, "fn renamed() {}\nfn renamed() {}\n");
    }

    #[test]
    fn ast_query_replacement_reports_an_invalid_query() {
        let err = apply_strategy(
            Some("fn a() {}"),
            &EditStrategy::Ast {
                transform: AstTransform::ReplaceQueryMatch {
                    query: "(this is not a valid".into(),
                    capture: "x".into(),
                    replacement: "y".into(),
                    all: false,
                },
            },
            &rust_context(),
        )
        .unwrap_err();
        assert!(matches!(err, EditError::Lang(_)));
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
            &plain(),
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
            &plain(),
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
            &plain(),
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
            &plain(),
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
            &plain(),
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
            &plain(),
        )
        .unwrap();
        assert_eq!(result, bigger);
    }
}
