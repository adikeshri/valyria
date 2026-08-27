# Roadmap and status

What is built, what is scaffolded, and what comes next. The phase definitions
and exit criteria live in [PLAN.md §5](PLAN.md#5-build-phases); this file is the
running status against them.

Status vocabulary:

- **done** — implemented and tested; the phase's exit criteria are met.
- **partial** — the phase's crates exist and work, but some listed capability is
  missing (noted inline).
- **scaffolded** — the crate compiles, declares its layer and phase, and is
  wired into the layering check. No implementation yet.

Last updated: 2026-08-28 (after Phase 3).

---

## Phases

| Phase | Scope | Status |
|---|---|---|
| 0 | Foundations: workspace, toolchain pin, CI, `types`, `util`, `store`, `events`, `config`, `testkit`, `xtask` | **done** |
| 1 | Platform: `vfs`, `process`, `sandbox`, `hardware`, `git` | **partial** — macOS and permissive sandboxes only; no Linux or Windows confinement |
| 2 | Execution: `permissions`, `tools`, `edit`, `ledger` | **partial** — 16 of 18 tools live; 3 of 6 edit strategies live (the rest need the index) |
| 3 ⭐ | Walking skeleton: `model` + `runtime-fake` + minimal `orchestrator`, `task`, `agent`, minimal `context`, `protocol`, `app`, `cli` | **done** |
| 4 | Repository intelligence: `lang`, `index`, `graph`, incremental pipeline, `lsp` | scaffolded |
| 5 | Search: `embed`, `search`, fusion ranking, explanations | scaffolded |
| 6 | Context, instructions, memory; prompt assembly with the trust lattice | scaffolded |
| 7 | Verification, diagnosis, repair: `verify`, failure parsers, repair loop, loop detection | scaffolded |
| 8 | Planning and multi-agent: `plan`, checkpoints, rollback boundaries, sub-tasks | scaffolded |
| 9 | Real models: `runtime-llamacpp`, `runtime-mlx`, `runtime-openai-compat`, registry, store, model pool | scaffolded |
| 10 | Interface completion: protocol v1 freeze, schema export, full CLI, TUI, `doctor`, `clean`, daemon | not started |
| 11 | Hardening and evaluation: `bench`, fuzzing, perf work, cross-platform matrix, release gates | not started |

Phases 4–5 and 7 are parallelizable now that 3 has landed. Phase 9 can start
early against the OpenAI-compatible adapter (a locally running `llama-server`)
without waiting for any FFI work.

---

## What Phase 3 actually delivered

`valyria run "<objective>"` against a fixture repo, driven by the deterministic
fake model, does all of the following — proven by
[crates/valyria-cli/tests/walking_skeleton.rs](../crates/valyria-cli/tests/walking_skeleton.rs),
which drives the real compiled binary as a separate OS process:

- reads a file, edits it, and runs a command, each through the permission engine;
- journals every state transition and effect before it runs;
- persists to `<workspace>/.valyria/workspace.db`;
- streams protocol events with a resumable cursor;
- survives a real `SIGKILL` and resumes from the journal on the next invocation;
- pauses, resumes and cancels from separate process invocations;
- parks in `WAITING_FOR_PERMISSION` and continues after
  `valyria task permission resolve`.

431 tests pass across the workspace.

---

## Known gaps

Things that exist as an interface but not yet as behaviour. Each returns a
clear "not implemented in this phase" error rather than pretending:

| Gap | Lands in |
|---|---|
| `search` and `symbol_search` tools return `tools.not_yet_implemented` | Phase 5 |
| Edit strategies 4 (symbol-aware) and 5 (AST transform) return `EditError::NotYetImplemented` | Phase 4 |
| Sandbox on Linux and Windows falls back to `PermissiveSandbox` (reported, never silent) | Phase 1 completion |
| Context assembly handles explicitly-named files only — no retrieval or ranking | Phase 6 |
| Only the fake model runtime exists; no real inference | Phase 9 |
| No planning, memory, instructions, verification or repair loop | Phases 6–8 |
| No `doctor`, `clean`, storage inspection, daemon mode, or TUI | Phase 10 |
| Protocol is unversioned in practice — no schema export or compat gate yet | Phase 10 |

---

## Open decisions

Defaults are chosen so work can proceed; changing one changes the plan
materially. Full context in [PLAN.md §10](PLAN.md#10-decisions-i-need-from-you).

1. **Windows tier** — default: tier 3, reduced sandbox, documented as such.
2. **llama.cpp integration mode** — default: managed `llama-server` subprocess
   rather than in-process FFI.
3. **Daemon vs embedded** — default: both, embedded in-process as the CLI default.
4. **Vector store** — default: in-house HNSW over the CAS.
5. **Team shape** — the phase graph assumes 3–5 parallel workstreams after Phase
   3; a solo build should defer Phases 5 and 8 and the MLX/CUDA adapters.
6. **Tier-1 language list** — 11 languages planned; cutting to 5 (Rust, TS/JS,
   Python, Go, Java) removes noticeable effort from Phases 4 and 7.
