# Benchmarks and evaluation

`valyria-bench` (layer 6, `crates/valyria-bench`) is the evaluation
harness from [PLAN.md §4.30](PLAN.md#430-benchmarks-and-evaluation-50).
This document is how to run it and how the CI gate works.

## The model

A benchmark task is `{ repo, objective, setup, oracle }`:

- **`repo`** — a `RepoSpec`: a set of files laid down into a fresh temp
  directory. (A pinned-commit real repository would be another `RepoSpec`
  variant; the offline CI suite only needs fixtures.)
- **`objective`** — the natural-language task, exactly as a user would
  type it into `valyria run`.
- **`setup`** — the fake-model `Scenario` (D12): the deterministic,
  turn-by-turn script that stands in for "what the agent decides to do",
  so a run has no nondeterminism and no network.
- **`oracle`** — an **executable** pass/fail check, never the model's own
  say-so (D4). The oracle types:

  | Oracle | Passes when |
  |---|---|
  | `CommandSucceeds { program, args }` | the command exits 0 in the finished workspace ("the tests pass") |
  | `ReportVerified` | the completion report's status is `Verified` |
  | `TaskCompleted` | the task reached `COMPLETED` |
  | `FileContains` / `FileLacks` / `FileExists` | a file's post-run content |
  | `MaxFilesChanged { max }` | at most `max` files changed (diff-size constraint) |
  | `PathsUntouched { paths }` | none of `paths` was modified |
  | `All(...)` | every sub-oracle passes |

Each graded run also yields `BenchMetrics` — wall time plus counts
projected from the task journal (model calls, tool calls, verification
runs, tests passed/failed, progress stalls). A `BenchReport` is a
serializable list of `BenchOutcome`s; `compare(baseline, current)` reports
task-level regressions (passed → fails, or newly missing) and cost-metric
blow-ups on tasks that still pass.

## The offline fixture suite

`valyria_bench::fixture_suite()` — one task per §4.30 category, all
runnable with the network down:

| Task | Category | What it exercises |
|---|---|---|
| `feature_add_function` | feature | read → exact-replacement edit → finish → COMPLETED |
| `bugfix_verified` | bug_fix | edit → driver-discovered `check.sh` verify → report `Verified` |
| `debugging_repair_loop` | debugging | wrong edit → verify fails → **Diagnosing → Repairing** → verify passes |
| `refactor_rename` | refactor | symbol rename, old name gone / new name present |
| `test_creation` | test_creation | `write_file` a new `tests/` file |
| `dependency_work` | dependency_work | edit a `deps.txt` manifest (a real `Cargo.toml` would make `Verifying` run `cargo test` — not hermetic) |
| `exploration_readonly` | exploration | list + read only, **zero files changed** |

## Running it

```bash
# run the suite, print a table (exit non-zero on any oracle failure)
cargo run -p valyria-bench -- run

# machine-readable
cargo run -p valyria-bench -- run --json

# compare a fresh run to a recorded baseline
cargo run -p valyria-bench -- run --baseline docs/bench/baseline.json

# (re)record the committed baseline
cargo run -p valyria-bench -- baseline docs/bench/baseline.json
```

Or through `xtask`, which is what CI calls:

```bash
cargo xtask bench            # run + compare to docs/bench/baseline.json, fail on regression
cargo xtask bench --bless    # re-record the baseline (do this when a change is intentional)
cargo xtask release-gates    # every machine-checkable gate in one run
```

`cargo xtask bench` is a job in `.github/workflows/ci.yml`; the
integration test `crates/valyria-bench/tests/suite.rs` runs the same
comparison inside `cargo test`.

## The recorded baseline

`docs/bench/baseline.json` is a `BenchReport` with wall-clock fields
zeroed (`compare` ignores timing), so the checked-in file is byte-stable
across runs while remaining a real pass/fail + cost-metric record. A
change that legitimately shifts a cost metric (an extra model call, say)
is landed by running `cargo xtask bench --bless` and committing the new
baseline in the same change.

## Performance budgets

`valyria_bench::perf` covers the [§9](PLAN.md#9-platform-and-performance-targets)
budgets that need only a runtime and a fixture (task open / resume
latency). It is wired as an `#[ignore]`d test so a noisy CI runner never
fails a build on a timing wobble:

```bash
cargo test -p valyria-bench --test perf -- --ignored --nocapture
```

The scale budgets — cold index of a 100k-file repo, incremental-update
p95, lexical/regex search at scale — need a pinned real-repo corpus and a
criterion setup, and are **not** claimed here. That corpus, plus a
SWE-bench-style external adapter and a run of the whole suite against a
*real* local model (needs a live `llama-server`), are the documented
Phase 11 follow-ups.
