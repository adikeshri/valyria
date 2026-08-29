//! Performance-budget smoke check (§9). `#[ignore]`d so a noisy CI runner
//! never fails a build on a timing wobble; run it explicitly with
//!
//!   cargo test -p valyria-bench --test perf -- --ignored --nocapture
//!
//! The scale budgets (100k-file cold index, incremental p95, search at
//! scale) are deliberately out of scope here — they need this crate's
//! future pinned-repo corpus.

use std::time::Instant;

use valyria_app::{Runtime, RuntimeConfig};
use valyria_bench::perf::{budget, PerfSample};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "timing-sensitive; run explicitly"]
async fn task_open_and_reopen_are_within_budget() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("lib.rs"), "pub fn a() {}\n").unwrap();
    let data = tempfile::tempdir().unwrap();
    let config = RuntimeConfig::new(ws.path()).with_data_dir(data.path().join("d"));

    let t0 = Instant::now();
    let rt = Runtime::open(config.clone()).await.unwrap();
    let cold = PerfSample {
        name: "runtime open (cold)",
        measured: t0.elapsed(),
        budget: budget::TASK_RESUME,
    };
    drop(rt);

    let t1 = Instant::now();
    let _rt2 = Runtime::open(config).await.unwrap();
    let warm = PerfSample {
        name: "runtime reopen (resume)",
        measured: t1.elapsed(),
        budget: budget::TASK_RESUME,
    };

    println!("{}", cold.render());
    println!("{}", warm.render());
    assert!(cold.met(), "cold open exceeded {:?}", cold.budget);
    assert!(warm.met(), "reopen exceeded {:?}", warm.budget);
}
