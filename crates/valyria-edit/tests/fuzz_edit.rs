//! Property fuzzing for the parser-backed edit strategies (§7 testing
//! strategy: "patch & diff parsers ... proptest"). The invariant is
//! total-function behaviour: for *any* input, `apply_strategy` returns
//! `Ok` or a typed `Err` — never a panic, and never an unbounded hang.

use proptest::prelude::*;
use valyria_edit::strategy::{apply_strategy, EditContext, EditStrategy};

proptest! {
    #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

    /// Arbitrary anchor / replacement / current-content triples.
    #[test]
    fn exact_replacement_never_panics(
        current in proptest::option::of(".{0,400}"),
        anchor in ".{0,80}",
        replacement in ".{0,80}",
    ) {
        let strat = EditStrategy::ExactReplacement { anchor: anchor.clone(), replacement };
        let ctx = EditContext::default();
        let _ = apply_strategy(current.as_deref(), &strat, &ctx);
    }

    /// Arbitrary text handed to the unified-diff parser (`diffy`). Most
    /// inputs are not valid patches; the contract is that they fail
    /// cleanly.
    #[test]
    fn unified_diff_parser_never_panics(
        current in ".{0,400}",
        diff in ".{0,400}",
    ) {
        let strat = EditStrategy::UnifiedDiff { diff };
        let ctx = EditContext::default();
        let _ = apply_strategy(Some(&current), &strat, &ctx);
    }

    /// Diff text built to *look* like a hunk header — exercises the
    /// parser's structured path, not just its reject path.
    #[test]
    fn diff_shaped_input_never_panics(
        minus in 0u32..50,
        plus in 0u32..50,
        body in prop::collection::vec("[ +-].{0,20}", 0..12),
    ) {
        let diff = format!(
            "--- a\n+++ b\n@@ -1,{minus} +1,{plus} @@\n{}\n",
            body.join("\n")
        );
        let strat = EditStrategy::UnifiedDiff { diff };
        let ctx = EditContext::default();
        let _ = apply_strategy(Some("line one\nline two\nline three\n"), &strat, &ctx);
    }

    /// A well-formed single-hunk patch applied to its own source must
    /// round-trip — the parser is not merely tolerant, it is correct on
    /// the good path.
    #[test]
    fn a_real_single_line_patch_applies(tail in "[a-z ]{1,20}") {
        let before = "alpha\nbeta\ngamma\n";
        let after = format!("alpha\nbeta {tail}\ngamma\n");
        let diff = format!(
            "--- a\n+++ b\n@@ -1,3 +1,3 @@\n alpha\n-beta\n+beta {tail}\n gamma\n"
        );
        let ctx = EditContext::default();
        let out = apply_strategy(Some(before), &EditStrategy::UnifiedDiff { diff }, &ctx);
        prop_assert_eq!(out.ok(), Some(after));
    }
}
