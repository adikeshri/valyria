# Architecture

How Valyria is put together, and why. This is the orientation document — read
it before changing anything structural. [PLAN.md](PLAN.md) is the long form:
full subsystem designs, phase sequencing, performance budgets and the risk
register. [ROADMAP.md](ROADMAP.md) tracks what is actually built.

---

## 1. The shape of the system

One task = one durable state machine, driven by a loop that turns model output
into permission-gated tool calls against a real repository, recording evidence
as it goes.

```
                        ┌──────────────┐
   valyria (CLI)  ─────►│  protocol    │  requests in, events out
                        └──────┬───────┘
                               │
                        ┌──────▼───────┐
                        │ valyria-app  │  composition root: opens the workspace,
                        │  (Runtime)   │  applies migrations, wires everything
                        └──────┬───────┘
                               │
        ┌──────────────────────┼───────────────────────┐
        │                      │                       │
  ┌─────▼─────┐        ┌───────▼────────┐      ┌───────▼───────┐
  │  task     │        │    agent       │      │ orchestrator  │
  │ manager   │◄──────►│  step driver   │─────►│  role routing │
  │ + journal │        │  (state m/c)   │      └───────┬───────┘
  └─────┬─────┘        └───────┬────────┘              │
        │                      │                ┌──────▼───────┐
        │                      │                │ model runtime │
        │              ┌───────▼────────┐       │ (fake today)  │
        │              │  permissions   │       └───────────────┘
        │              │ → Authorization│
        │              └───────┬────────┘
        │                      │
        │              ┌───────▼────────┐
        │              │  tool runtime  │──► vfs / process+sandbox / git / edit
        │              └───────┬────────┘
        │                      │
  ┌─────▼──────────────────────▼────────┐
  │ store (SQLite actor + CAS) · events │
  └─────────────────────────────────────┘
```

Every arrow that crosses into "does something to the machine" passes through
the permission engine first, and everything that happened is in the journal
afterwards.

---

## 2. Layers

Crates depend strictly downward. Each crate declares its tier in its
`Cargo.toml`:

```toml
[package.metadata.valyria]
layer = 3
phase = 2
```

`cargo run -p xtask -- check-layering` reads that metadata plus each crate's
`[dependencies]` and fails on any upward edge or same-layer cycle. It runs in
CI, so the layering is a mechanism, not a convention. `[dev-dependencies]` are
exempt (that is how every layer can use `valyria-testkit`).

### Layer 0 — Foundation
`valyria-types` · `valyria-util` · `valyria-store` · `valyria-events` ·
`valyria-config` · `valyria-testkit`

IDs, the trust lattice, the error taxonomy, the SQLite single-writer actor and
content-addressed blob store, the sequenced event bus, layered config
resolution, and the test harness. No I/O in `types`.

### Layer 1 — Platform
`valyria-vfs` · `valyria-process` · `valyria-sandbox` · `valyria-hardware` ·
`valyria-git`

Workspace-rooted filesystem access with atomic writes and content hashing;
process spawn/supervision with output caps, timeouts and process-group kill;
sandbox traits with per-platform implementations; hardware probing; `gix`-backed
git reads.

### Layer 2 — Repository intelligence *(`embed` and `search` scaffolded, Phase 5)*
`valyria-lang` · `valyria-lsp` · `valyria-index` · `valyria-graph` ·
`valyria-embed` · `valyria-search`

Language support as data behind one trait — a tree-sitter grammar plus a
directory of `.scm` queries per language, and a single extraction engine driven
by capture names. A generational file/symbol index whose rows record the
generation range they were valid for, so a read at generation *N* always sees
the repository as it was then. A typed knowledge graph derived from one index
generation, with confidence on every edge that name-based resolution could get
wrong. An LSP client pool that enriches those answers where a server exists and
degrades silently where one does not.

### Layer 3 — Execution
`valyria-permissions` · `valyria-tools` · `valyria-edit` · `valyria-ledger` ·
`valyria-verify` *(verify scaffolded, Phase 7)*

Permission modes and rule evaluation, the tool registry and invocation records,
the editing strategy ladder, and the change ledger.

### Layer 4 — Model
`valyria-model` · `valyria-model-registry` · `valyria-model-store` ·
`valyria-runtime-fake` · `valyria-runtime-llamacpp` · `valyria-runtime-mlx` ·
`valyria-runtime-openai-compat` · `valyria-orchestrator`

The `ModelRuntime` trait and its adapters. Only the deterministic fake adapter
is implemented; the rest are scaffolded for Phase 9.

### Layer 5 — Agent
`valyria-context` · `valyria-instructions` · `valyria-memory` · `valyria-plan` ·
`valyria-agent` · `valyria-task`

The step machine and its driver, the task manager and journal, and (in later
phases) context assembly, instructions, memory and planning.

### Layer 6 — Interface
`valyria-protocol` · `valyria-app` · `valyria-cli` · `valyria-bench` · `xtask`

---

## 3. Load-bearing decisions

These are the choices the rest of the design hangs off. Full rationale in
[PLAN.md §1](PLAN.md#1-design-decisions-that-shape-everything); the short
version:

**D1 — The agent loop is a persisted state machine, not a call stack.**
`step(state, input) -> (state', effects)` is pure and synchronous; a driver
executes the effects. Every step appends to an append-only journal *before* its
effects run, and effect completion is recorded. Resume = load the last snapshot,
replay the journal tail, re-issue any effect that was issued but never
completed (each carries an idempotency key). Pause/resume, crash survival,
audit, event streaming and deterministic replay tests all fall out of this one
decision.

**D2 — Authorization is an unforgeable capability, not a boolean.**
`Tool::execute` takes an `Authorization` whose constructor is private to
`valyria-permissions` and which is bound to
`(task_id, step_id, tool, canonical_input_hash, expiry)`. No code path executes
a tool without one, and neither the agent, a model response, nor a tool can mint
one. Input-hash binding kills TOCTOU: approval for `rm ./tmp` cannot be spent on
`rm -rf /`.

**D3 — Every byte of context carries provenance and a trust level.**
`Trust` is ordered: `Policy > Instruction > Evidence > RepoData > ModelOutput`.
Prompt assembly is the only place that turns `ContextItem`s into a prompt, and
nothing below `Instruction` may occupy a system/policy position. Anything at
`Evidence` or below is nonce-fenced and framed as data, not instructions.
Injection defense is then a property of one function with exhaustive tests.

**D4 — Model claims are never evidence.** Completion reports are generated only
from `Evidence` rows, which models cannot construct. If the model says "tests
pass" and no verification run exists, the report says "not verified".

**D5 — Local models need a tool-call transport ladder.** Native tool-call API →
grammar-constrained decoding → fenced-JSON with a tolerant recovery parser,
chosen per model from capabilities probed at install time.

**D6 — Writes are optimistic-concurrency operations.** Every write carries the
content hash the agent believes the file has. A mismatch fails with
`ExternalModification` and becomes an agent observation rather than a clobber.

**D7 — SQLite is the state substrate; blobs are content-addressed.** One
`workspace.db` per repository behind a single-writer actor; large payloads
(stdout, transcripts, embeddings) go to a `blake3`-keyed CAS.

**D8 — Indexes are generational and immutable per generation.** A long step
never sees the index mutate underneath it, and a step records the generation it
planned against so staleness is detectable.

**D9 — Language support is data plus a trait**, never a `match` on file
extension.

**D10 — Everything sandboxable goes through `ProcessLauncher` and `FsGuard`.**
A `PermissiveSandbox` exists where confinement is unavailable, but the runtime
*reports* its actual confinement level rather than silently degrading.

**D11 — The protocol is the only API surface.** The CLI is a protocol client
against an embedded runtime (or, later, a daemon). This is why the CLI cannot
grow orchestration logic: it cannot depend on the crates that would let it.

**D12 — The deterministic fake model is core infrastructure.**
`valyria-runtime-fake` ships as a first-class adapter with a scenario format
(turn-by-turn scripts, including malformed output and tool-call storms). Nearly
all agent tests run against it.

---

## 4. The life of a task

1. **Create.** `TaskCreate` over the protocol. `valyria-app` opens
   `<workspace>/.valyria/workspace.db`, applies every crate's migrations, and
   registers the task. The task starts in `Idle`.
2. **Step.** The driver asks the orchestrator for the next action, given the
   assembled context. The state machine computes the transition; the transition
   is journaled before anything runs.
3. **Authorize.** The requested tool's `preflight` produces a
   `PermissionRequest`. The engine evaluates workspace rules, user rules, mode
   defaults and the compiled-in policy floor, and returns `Allow`, `Deny` or
   `Ask`. `Ask` moves the task to `WaitingForPermission` and emits an event; the
   client answers with `PermissionResolve`.
4. **Execute.** With an `Authorization` in hand, the tool runs — under a sandbox
   profile derived from the permission decision, not chosen ad hoc. A full
   `ToolInvocationRecord` is written. Output is dual-form: structured for the
   runtime, rendered and budget-capped for the model.
5. **Record.** Edits go through the ledger (before/after hashes, patch blob) so
   they can be classified later as agent-authored, pre-existing, or a concurrent
   user modification — and so they can be rolled back.
6. **Repeat** until the model finishes, the task fails, or it is paused or
   cancelled. Terminal states (`Completed`, `Failed`, `Cancelled`) have no
   outgoing transitions.

The fourteen states and the legal transitions between them live in
[crates/valyria-types/src/state.rs](../crates/valyria-types/src/state.rs), as a
pure exhaustively-tested function.

### Events

Events are projections of the journal, not a parallel mechanism. Subscribers
pass a `since: Seq` cursor, so a client that reconnects gets exactly what it
missed. Per-subscriber buffers are bounded and overflow surfaces as an explicit
`Lagged { dropped, resume_from }` event rather than silent loss.

---

## 5. Cross-cutting conventions

- **Errors** — `thiserror` per crate, no `anyhow` below layer 6. Every error
  carries a stable `code` and a `retryable` flag. Errors that reach the model go
  through redaction first.
- **Async** — pure logic (state machine, ranking, parsing, planning) is
  synchronous and unit-testable; async lives in drivers and adapters. CPU-heavy
  work runs on `rayon` behind `spawn_blocking` with explicit concurrency caps.
- **Cancellation** — one `CancellationToken` tree per task, propagated into
  every process, model call and query. Every long-running operation has a
  "cancel mid-flight and assert cleanup" test.
- **IDs** — ULID-based typed newtypes, prefixed on display (`task_01H…`).
- **Determinism** — `Clock`, `Rng` and `IdGen` are injected traits. That is what
  makes journal-replay tests meaningful.
- **Observability** — `tracing`, one span per step / tool call / model call, with
  span IDs recorded in the journal so a log line ties back to a protocol event.

---

## 6. On-disk layout

```
~/.valyria/                 # global
  config.toml
  models/<model-id>/        # weights, manifest, license, probe results
  registry/catalog.json
  blobs/<bl>/<ake3>...      # content-addressed store
  global.db                 # models, user memory, workspace registry
  logs/

<repo>/.valyria/            # per workspace, gitignored
  workspace.db              # tasks, journal, ledger, evidence, index metadata
  index/                    # generation-tagged index artifacts
  cache/
  tasks/<task-id>/          # large artifacts, transcripts, diffs
```

Migrations are forward-only, versioned, and owned by the crate whose tables they
create; `valyria-app` composes them at startup. Every store is meant to
implement `inspect() -> StorageReport` and `purge(scope)` so that inspection and
deletion are first-class rather than "go delete a directory".

---

## 7. Testing

The bulk of orchestration coverage comes from scenario scripts driven through
the fake model — deterministic and fast. Beyond that: unit tests for pure logic,
property/fuzz tests for the patch, diff and protocol parsers, per-tool tests
against fixture workspaces including permission-denied paths, sandbox
escape-attempt corpora, journal replay tests, and one end-to-end test that
drives the compiled binary through `SIGKILL` and resume.

See [CONTRIBUTING.md](../CONTRIBUTING.md) for what to write when you add code,
and [PLAN.md §7](PLAN.md#7-testing-strategy-51) for the full strategy.
