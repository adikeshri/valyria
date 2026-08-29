//! The offline fixture suite is the Phase 11 orchestration regression
//! guard: every task drives a *real* `valyria_app::Runtime` bound to the
//! deterministic fake model, with no network, and is graded by an
//! executable oracle.

use valyria_bench::{compare, fixture_suite, BenchReport, BenchRunner};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fixture_suite_passes_end_to_end_offline() {
    let report = BenchRunner::new()
        .run_suite(&fixture_suite())
        .await
        .expect("suite runs");

    assert!(
        report.all_passed(),
        "fixture suite had failures:\n{}",
        report.render_table()
    );
    // Every §4.30 category is represented.
    assert_eq!(report.by_category().len(), 7);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_run_is_clean_against_the_committed_baseline() {
    let baseline_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/bench/baseline.json"
    );
    let baseline = BenchReport::from_json(
        &std::fs::read_to_string(baseline_path).expect("committed baseline exists"),
    )
    .expect("baseline parses");

    let fresh = BenchRunner::new()
        .run_suite(&fixture_suite())
        .await
        .expect("suite runs");

    let cmp = compare(&baseline, &fresh);
    assert!(
        cmp.is_clean(),
        "current run regressed against docs/bench/baseline.json:\n{}\n\nre-record with `cargo xtask bench --bless` if the change is intentional",
        cmp.render()
    );
}
