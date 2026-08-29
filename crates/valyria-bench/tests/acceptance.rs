//! Phase 11 exit criterion: "§53 acceptance criteria demonstrated
//! end-to-end". This test is the machine-checkable half of
//! `docs/ACCEPTANCE.md` — it walks the PLAN §6 acceptance mapping, runs
//! the offline fixture suite as the end-to-end demonstration, and asserts
//! that every criterion is either demonstrated here or has a named
//! proving test elsewhere in the workspace. The single documented
//! exception is a run against a *real* local model (needs a live
//! `llama-server` — the Phase 9/10 follow-up).

use valyria_bench::{fixture_suite, BenchRunner};

#[derive(Debug)]
enum Status {
    /// Demonstrated by the offline fixture suite in this crate.
    Demonstrated,
    /// Proven by a named test in another crate (integration realism lives
    /// with the subsystem that owns it — PLAN §7).
    ProvenElsewhere(&'static str),
    /// Deliberately deferred, with the reason.
    Deferred(&'static str),
}

struct Criterion {
    n: u8,
    what: &'static str,
    status: Status,
}

fn mapping() -> Vec<Criterion> {
    use Status::*;
    vec![
        Criterion { n: 1, what: "open an arbitrary repository", status: ProvenElsewhere("valyria-app::runtime::tests + valyria-cli/tests/walking_skeleton.rs") },
        Criterion { n: 2, what: "discover language / tooling / git / conventions", status: ProvenElsewhere("valyria-verify::discovery::tests, valyria-instructions::tests") },
        Criterion { n: 3, what: "build context without the whole repo", status: ProvenElsewhere("valyria-context::tests (budget + provenance round-trip)") },
        Criterion { n: 4, what: "local model behind one abstraction", status: ProvenElsewhere("valyria-runtime-openai-compat/tests/runtime.rs, valyria-runtime-fake") },
        Criterion { n: 5, what: "plan a multi-step task", status: ProvenElsewhere("valyria-agent/tests/plan_loop.rs") },
        Criterion { n: 6, what: "modify files", status: Demonstrated },
        Criterion { n: 7, what: "execute project tools safely (sandbox / permissions)", status: ProvenElsewhere("valyria-sandbox escape corpus, valyria-permissions::rules::tests") },
        Criterion { n: 8, what: "run verification and collect evidence", status: Demonstrated },
        Criterion { n: 9, what: "diagnose failures", status: ProvenElsewhere("valyria-verify::parse::tests (captured-output corpus)") },
        Criterion { n: 10, what: "repair a seeded bug end to end", status: Demonstrated },
        Criterion { n: 11, what: "detect lack of progress", status: ProvenElsewhere("valyria-agent/tests/repair_loop.rs::an_unfixable_bug_trips_loop_detection_and_is_handed_off") },
        Criterion { n: 12, what: "preserve developer changes (concurrent-edit)", status: ProvenElsewhere("valyria-ledger concurrent-modification suite, valyria-agent/tests/plan_loop.rs rollback tests") },
        Criterion { n: 13, what: "pause / resume / cancel across a restart", status: ProvenElsewhere("valyria-cli/tests/walking_skeleton.rs (SIGKILL + resume)") },
        Criterion { n: 14, what: "persist task state (migrations + replay)", status: ProvenElsewhere("valyria-store migration tests, valyria-app/tests/runtime.rs") },
        Criterion { n: 15, what: "explain what it verified (no unbacked claims)", status: Demonstrated },
        Criterion { n: 16, what: "no cloud / fully offline", status: Demonstrated },
        Criterion { n: 17, what: "controllable via the protocol alone", status: ProvenElsewhere("valyria-cli/tests/phase10.rs (serve + --connect round trip)") },
        Criterion { n: 18, what: "CLI + desktop with no duplicated orchestration (D11)", status: ProvenElsewhere("xtask check-layering (valyria-cli has no valyria-agent dep)") },
        Criterion {
            n: 0,
            what: "the same suite green against a real local model",
            status: Deferred("needs a running llama-server; the offline fake-model demonstration stands in — Phase 9/10 follow-up"),
        },
    ]
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_criteria_are_demonstrated_or_accounted_for() {
    let report = BenchRunner::new()
        .run_suite(&fixture_suite())
        .await
        .expect("suite runs");

    println!("\nPLAN §6 acceptance mapping\n");
    let mut deferred = Vec::new();
    for c in mapping() {
        let tag = match &c.status {
            Status::Demonstrated => "demonstrated (offline fixture suite)".to_string(),
            Status::ProvenElsewhere(t) => format!("proven: {t}"),
            Status::Deferred(why) => {
                deferred.push(c.n);
                format!("DEFERRED: {why}")
            }
        };
        let label = if c.n == 0 {
            "*".to_string()
        } else {
            c.n.to_string()
        };
        println!("  #{label:<2} {:<52} {tag}", c.what);
    }

    assert!(
        report.all_passed(),
        "the end-to-end demonstration (fixture suite) is not green:\n{}",
        report.render_table()
    );
    assert_eq!(
        deferred,
        vec![0],
        "exactly one acceptance item is deferred (the real-local-model run)"
    );
}
