# Acceptance criteria — status

The [PLAN.md §6](PLAN.md#6-acceptance-mapping) acceptance mapping, with the
concrete test that proves each criterion. The machine-checked version of
this table is `crates/valyria-bench/tests/acceptance.rs`, which runs the
offline fixture suite as the end-to-end demonstration and asserts every
row below is either demonstrated there or has a named proving test.

Status vocabulary:

- **demonstrated** — shown end-to-end by the offline `valyria-bench`
  fixture suite (`crates/valyria-bench/src/suite.rs`), graded by an
  executable oracle.
- **proven** — covered by a named test in the crate that owns the
  behaviour (PLAN §7: integration realism lives with the subsystem).

| # | Criterion | Status | Proof |
|---|---|---|---|
| 1 | Open an arbitrary repository | proven | `valyria-app/tests/runtime.rs`, `valyria-cli/tests/walking_skeleton.rs` |
| 2 | Discover language / tooling / git / conventions | proven | `valyria-verify::discovery::tests`, `valyria-instructions::tests` |
| 3 | Build context without the whole repo | proven | `valyria-context/tests` (budget assertions + provenance byte-round-trip) |
| 4 | Local model behind one abstraction | proven | `valyria-runtime-openai-compat/tests/runtime.rs`, `valyria-runtime-fake` |
| 5 | Plan a multi-step task | proven | `valyria-agent/tests/plan_loop.rs` |
| 6 | Modify files | **demonstrated** | `feature_add_function`, `refactor_rename`, `test_creation`, `dependency_work` |
| 7 | Execute project tools safely (sandbox / permissions) | proven | `valyria-sandbox` escape corpus, `valyria-permissions::rules::tests` |
| 8 | Run verification and collect evidence | **demonstrated** | `bugfix_verified` (driver-discovered `check.sh`, report `Verified`) |
| 9 | Diagnose failures | proven | `valyria-verify::parse::tests` (captured-output corpus) |
| 10 | Repair a seeded bug end to end | **demonstrated** | `debugging_repair_loop` (verify fails → Diagnosing → Repairing → verify passes) |
| 11 | Detect lack of progress | proven | `valyria-agent/tests/repair_loop.rs::an_unfixable_bug_trips_loop_detection_and_is_handed_off` |
| 12 | Preserve developer changes (concurrent edit) | proven | `valyria-ledger` concurrent-modification suite, `valyria-agent/tests/plan_loop.rs` rollback tests |
| 13 | Pause / resume / cancel across a restart | proven | `valyria-cli/tests/walking_skeleton.rs` (real `SIGKILL` + resume) |
| 14 | Persist task state (migrations + replay) | proven | `valyria-store` migration tests, `valyria-app/tests/runtime.rs` |
| 15 | Explain what it verified (no unbacked claims) | **demonstrated** | `bugfix_verified` / `debugging_repair_loop` assert `ReportVerified` from durable runs only |
| 16 | No cloud / fully offline | **demonstrated** | the whole fixture suite runs with the network down; CI `offline` job |
| 17 | Controllable via the protocol alone | proven | `valyria-cli/tests/phase10.rs` (`serve` + `--connect` round trip) |
| 18 | CLI + desktop with no duplicated orchestration (D11) | proven | `cargo xtask check-layering` (`valyria-cli` has no `valyria-agent` dependency) |

## Deferred

One acceptance item from PLAN Phase 11's exit criteria is deliberately
deferred, consistent with the Phase 9/10 scope choices:

- **The same suite green against a real local model.** Needs a running
  `llama-server` and the concrete `reqwest` `HttpTransport`. The offline
  fake-model demonstration stands in; every subsystem the real path
  depends on is proven independently (`OpenAiCompatRuntime` against
  `MockTransport`, the transport ladder's recovery/retry corpus).
