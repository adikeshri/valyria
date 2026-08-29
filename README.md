# Valyria

A local-first coding agent runtime, written in Rust.

Valyria plans, edits, runs and verifies work inside a real repository using
models that run on your own machine. There is no cloud service, no telemetry,
and no network dependency at runtime — the offline test job in CI exists to
keep it that way.

> **Status: early.** The workspace is a ~40-crate skeleton in which phases 0–11
> of [the build plan](docs/PLAN.md) are implemented (Phase 11 as an offline
> slice: the `valyria-bench` evaluation harness, an executable-oracle fixture
> suite that is a CI regression gate, property/fuzz suites, and a machine-checked
> [acceptance mapping](docs/ACCEPTANCE.md) — a run against a *real* local model
> and the large-repo performance corpus remain). The end-to-end agent loop runs
> today against a deterministic **fake** model; the real model runtimes
> (llama.cpp, MLX) landed as documented scaffolds in Phase 9, behind a working
> OpenAI-compatible adapter. The runtime is drivable through a frozen v1
> protocol — embedded, over a Unix-socket daemon, from the full CLI, or from the
> TUI (`valyria` with no arguments). See [docs/ROADMAP.md](docs/ROADMAP.md) for
> what is and is not built.

---

## What exists today

`valyria run` drives a complete, durable agent loop end to end:

- reads and edits files through a permission-gated tool runtime,
- runs commands in a sandboxed child process,
- journals every state transition and effect to SQLite **before** it runs,
- streams events over the protocol to its client,
- survives `kill -9` and resumes from the journal,
- and can be paused, resumed and cancelled from a separate process.

That path is covered by an integration test that drives the real compiled
binary against a real git fixture repo
([crates/valyria-cli/tests/walking_skeleton.rs](crates/valyria-cli/tests/walking_skeleton.rs)).

Around it sits the full interface: `valyria task list|status|report|plan|rollback`,
`valyria doctor` (a ten-check environment diagnosis), `valyria clean` and
`valyria status` (storage inspection), `valyria config` / `model list` /
`memory list`, a `valyria serve` daemon that speaks the same frozen v1 protocol
over a Unix socket (`--connect <socket>` on any command), and an interactive
TUI (`valyria` with no arguments). The wire contract is exported as JSON Schema
under [docs/protocol/](docs/protocol/) and a CI gate fails any unversioned
change to it.

Underneath it, the repository-intelligence layer understands the code rather
than just its bytes:

- **parses** Rust, Python, Go, Java, JavaScript, TypeScript and TSX into
  symbols, imports, call sites and tests, from a per-language directory of
  tree-sitter queries rather than from language-specific code;
- **indexes** them generationally, so a long agent step never has the index
  shift underneath it, and checks itself for drift by rebuilding independently
  and diffing;
- **relates** them in a typed knowledge graph — who calls this, what does a
  change here affect, which tests cover it — with a confidence on every edge
  that name-based resolution could get wrong;
- **enriches** all of that from a language server when one is installed, and
  works exactly as well when none is;
- **searches** it seven ways — lexical, regex, symbol, semantic, AST,
  dependency and git — fused into one ranked list where every hit explains its
  own score, over generational chunk embeddings behind an `Embedder` trait
  (a deterministic offline embedder stands in until a real model lands in
  Phase 9), and still works with embeddings switched off.

On top of that sits the context pipeline: a task and a repository become a
trust-ordered prompt where only policy and authorized instructions take a
system position, everything else is nonce-fenced as data with
instruction-shaped text flagged (not stripped), the token budget fails loudly
rather than truncating mid-symbol, and the whole prompt can be rebuilt
byte-for-byte from a stored snapshot. Instruction discovery has a fixed
authority order; memory is four decaying, inspectable tiers.

It also **verifies**: discovers the repository's own build/test/lint commands
(manifests, `Makefile`/`justfile` targets, CI `run:` steps), confirms each by
running it, escalates them cheapest-first with a mandatory full run before
completion, parses the failures of ten common tools into a structured
diagnosis, and persists every run as the `Evidence` the completion report is
built from — an unbacked "tests pass" is reported as *not verified*. On a
failure the driver runs a bounded **repair** loop (diagnose → one edit →
re-verify) under a loop detector — exact repeat, `A→B→A` oscillation, repeated
failure, no-change, stalled frontier — that escalates and then hands off rather
than spinning silently.

Not yet built: planning and every real model adapter. Retrieval into the
*live* agent loop is a `Retriever` seam with a search-backed implementation
ready but not yet wired in — nothing calls the index bootstrap during a task
yet, the `search` tool is still a stub, and the repair loop's suspect-ranking
sees the change ledger but not yet the graph.

## Requirements

- Rust `1.97.1` (pinned in [rust-toolchain.toml](rust-toolchain.toml); `rustup`
  picks it up automatically)
- `git` on `PATH`
- macOS or Linux. macOS aarch64 and Linux x86_64 are tier 1; Windows builds and
  tests in CI but has no sandbox implementation yet (see
  [SECURITY.md](SECURITY.md)).

## Build and run

```bash
cargo build --workspace
```

Run the bundled walking-skeleton scenario against a scratch repo:

```bash
cargo run -p valyria-cli -- run "add a function" --workspace /path/to/repo --events
```

The default scenario tells the fake model to read `src/lib.rs`, append a
function to it, run a command, and finish. Point `--workspace` at a throwaway
git repo — the agent really does edit files.

### CLI surface

```
valyria run "<objective>" [--workspace <path>] [--scenario <file.toml>]
                          [--permission-mode manual|assisted|autonomous] [--events]
valyria task status <task_id>   [--workspace <path>]
valyria task pause  <task_id>   [--workspace <path>]
valyria task resume <task_id>   [--workspace <path>]
valyria task cancel <task_id>   [--workspace <path>]
valyria task permission resolve <task_id> (--allow|--deny) [--workspace <path>]
```

`--events` prints every protocol event as JSON as it is emitted.
`--scenario` loads a fake-model script; see
[crates/valyria-runtime-fake/scenarios/walking_skeleton.toml](crates/valyria-runtime-fake/scenarios/walking_skeleton.toml)
for the format.

The CLI is a *protocol client*, not an agent. It links only
`valyria-app`, `valyria-protocol`, `valyria-types` and `valyria-util`, and the
layering check in CI fails if that ever stops being true.

### Permission modes

| Mode | Behaviour |
|---|---|
| `manual` | Ask before every mutating action. |
| `assisted` (default) | Auto-allow reads and workspace-scoped safe commands; ask for writes outside the plan scope, installs, network and destructive operations. |
| `autonomous` | Auto-allow inside the workspace and plan scope; still ask for destructive operations, network, git history rewrites and out-of-workspace access. |

A task that needs approval parks in `WAITING_FOR_PERMISSION` and emits an
event; answer it with `valyria task permission resolve <task_id> --allow`.

## Where state lives

```
<repo>/.valyria/            # per workspace, gitignored
  workspace.db              # tasks, journal, ledger, evidence, index metadata
  index/  cache/  tasks/    # generation-tagged artifacts (later phases)
~/.valyria/                 # global: config, models, blobs, logs (later phases)
```

Deleting `<repo>/.valyria` discards all agent state for that repository.

## Repository layout

```
crates/valyria-*   ~40 crates in six strictly-layered tiers
xtask/             layering check and release gates
docs/PLAN.md       the full engineering plan (design decisions, phases, budgets)
docs/ARCHITECTURE.md  how the pieces fit together
docs/ROADMAP.md    per-phase status
```

## Development

```bash
cargo test --workspace              # 1087 tests as of Phase 11
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- check-layering
cargo run -p xtask -- bench          # offline evaluation suite vs. the recorded baseline
cargo run -p xtask -- release-gates  # every machine-checkable gate, one summary
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions, the layering rule, and
what CI enforces.

## Documentation

| Document | What it covers |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate topology, the load-bearing design decisions, task lifecycle |
| [docs/PLAN.md](docs/PLAN.md) | The full build plan: subsystem designs, phases, acceptance mapping, risks |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Phase-by-phase status |
| [docs/BENCHMARKS.md](docs/BENCHMARKS.md) | The `valyria-bench` evaluation harness, the fixture suite, the CI gate |
| [docs/ACCEPTANCE.md](docs/ACCEPTANCE.md) | The PLAN §6 acceptance criteria and the test that proves each |
| [CONTRIBUTING.md](CONTRIBUTING.md) | Setup, conventions, review expectations |
| [SECURITY.md](SECURITY.md) | Threat model, current confinement guarantees, reporting |

## License

Apache-2.0. See [LICENSE](LICENSE).
