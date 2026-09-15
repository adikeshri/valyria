# Valyria — Completion Plan (Core + App)

The plan to take every partial, deferred, scaffolded and "follow-up" item in
Core ([PLAN.md](PLAN.md), [ROADMAP.md](ROADMAP.md)) and the app
(`valyria-app/docs/IMPLEMENTATION-PLAN.md`) to **finished, production
quality** — no MVP slices, no "offline slice", no documented deferrals left
in the acceptance table. Every milestone ships the Core change **and** the
app wiring together, so nothing lands in Core that the app can't use the same
day.

Written 2026-09-15 against Core `feat/local-inference` @ `1c139aa` (protocol
1.12.0, 1207 tests) and valyria-app `main` pinned to Core `8ba16cb`.

---

## 0. Ground truth — what is actually unfinished

ROADMAP.md was last updated after Phase 11 (2026-08-29) and is stale in both
directions: several gaps it lists have closed (G1–G15, `HttpFetcher`,
llama-server inference, `ServerProber`, the Windows named pipe), while some
real gaps are no longer called out. The list below was re-verified against the
code; each item names its evidence.

### 0.1 Core — agent loop and repository intelligence

| # | Gap | Evidence |
|---|---|---|
| C1 | Live loop drives through the Phase-3 `Orchestrator`; the transport ladder (`structured::resolve_tool_calls`), `RoleRouter` and `ModelPool` are built but unused | `valyria-orchestrator/src/lib.rs:17`, `valyria-agent/src/driver.rs:38`, `valyria-app/src/runtime.rs:290` |
| C2 | No grammar-constrained tier in the ladder (D5 tier 2); `supports_grammar` is always false | `model_runtimes.rs` capabilities, `structured.rs` |
| C3 | Context retrieval in the live loop is `StaticRetriever::empty()` — `SearchRetriever` is never used; memory never reaches the prompt | `driver.rs:306-313` |
| C4 | The index is only built on explicit `index_build`; no bootstrap on open, the VFS watcher has **zero consumers**, so no incremental indexing and no generation pinning per step | `runtime.rs:808-816`; `grep Watcher` outside `valyria-vfs` = none |
| C5 | `search` / `symbol_search` return `tools.not_yet_implemented` and are hidden from the model; `git_blame` is a stub | `valyria-tools/src/tools/search.rs:20-76`, `tools/git.rs:302`, `valyria-agent/src/tool_specs.rs:12` |
| C6 | Nine PLAN §4.10 tools don't exist: `apply_patch`, `read_many`, `find_references`, `find_definition`, `git_commit` (+ branch/stash/checkout), `memory_write`, `plan_update`, `ask_user`, `report_finding` | registry = 18 tools, `valyria-tools/src/tools/` |
| C7 | Diagnosis gets an empty graph-neighbour set; no changed-symbol → covering-test mapping for `TargetedTest` | `driver.rs:674-677` |
| C8 | Verify state, loop detector and repair ledger are process-local — lost across a restart | `driver.rs:109-112` |
| C9 | `PlanningMode::Passthrough` is the default; no child-task sub-agents, no parallel wave executor, plan targets resolved against the filesystem not the index, no per-step verification | `driver.rs:99-107`, `valyria-plan/src/roles.rs:10`, `validate.rs:10` |
| C10 | `valyria-lsp` has **no consumer crate** — LSP enrichment never runs | only `valyria-lsp/Cargo.toml` names it |
| C11 | Semantic search uses `HashingEmbedder` only; no Embedder/Reranker role model | `runtime.rs:857` |
| C12 | Memory: no automatic extraction after verified tasks in the live loop, `memory_list` with no query returns `[]`, no write/delete over the protocol | `runtime.rs:868-876` |
| C13 | Secret scanning is `valyria-util::redact` only; PLAN §4.29 wants entropy + pattern scanning on everything entering context and logs, with an event | `valyria-util/src/redact.rs` |
| C14 | Uncommitted: `Implementing → Completed` for no-change turns (driver, state table, regression test) | `git status` |

### 0.2 Core — model platform

| # | Gap | Evidence |
|---|---|---|
| M1 | `valyria-runtime-mlx` is a 26-line scaffold | `valyria-runtime-mlx/src/lib.rs` |
| M2 | Only one llama.cpp engine build is resolved; no CUDA/ROCm/Vulkan variant selection from hardware | `valyria-engine-store` |
| M3 | No registration of an existing local OpenAI-compatible endpoint (Ollama, LM Studio, vLLM) | protocol has no endpoint op |
| M4 | Catalog is embedded only; no signed remote refresh | `valyria-model-registry` |
| M5 | Pool admission uses card-declared footprint, not the probe's measured footprint; `ResourcePressure` never reaches a client | `pool.rs`, event kinds |
| M6 | No KV-cache prefix-stability guarantee (stable system/tool prefix, `cache_prompt`) | driver message assembly |

### 0.3 Core — platform, languages, interface, evaluation

| # | Gap | Evidence |
|---|---|---|
| P1 | Linux and Windows sandboxes fall back to `PermissiveSandbox` | `valyria-sandbox/src/launcher.rs:26-34` |
| P2 | `doctor` reports known confinement, doesn't run a live self-test | `valyria-app/src/doctor.rs:220` |
| P3 | No escape-attempt corpus per platform; no malicious-repo corpus; no `docs/SECURITY.md` threat model | PLAN §4.6, §4.29 |
| L1 | Six grammars (`rust python go java javascript typescript`) of the 11 tier-1; no tier-2 set | `valyria-lang/queries/`, `Cargo.toml:33` |
| L2 | Verify discovery/parsers cover only those ecosystems | `valyria-verify` |
| I1 | Daemon is single-workspace; no `workspace_open` / `workspace_close` | `daemon::serve` |
| I2 | Newline framing only (no negotiated length-prefix); no TypeScript export from `xtask schema` | `valyria-protocol/src/transport` |
| I3 | Missing protocol ops: `task_respond` (answer `WaitingForUser`), structured context on `task_create`, `memory_*` writes, `context_explain`, `permission_rules`, `bench_run`, `task_children` / `task_artifacts` / plan revisions | `Request` enum (37 variants) |
| I4 | CLI lacks PLAN §4.28's `search`, `index`, `review`, `fix`, `explain`, `benchmark` | `valyria-cli/src/main.rs:40-49` |
| E1 | Bench runs the fake model only; `acceptance.rs` carries one documented deferral | `valyria-bench/tests/acceptance.rs:53` |
| E2 | No scale corpus: 100k-file cold index, incremental p95, search p95 unmeasured; `verify-index` fuzz is 10 rounds, not 10k ops | ROADMAP "Phase 4 exit criteria not yet met" |
| E3 | No 30-repo discovery corpus, no captured-output parser corpus, no SWE-bench adapter, no `cargo-fuzz` targets, LSP never run against real servers | ROADMAP Phase 7/11 |
| E4 | `walking_skeleton.rs` flakiness | known, predates Phase 11 |

### 0.4 App

| # | Gap | Evidence |
|---|---|---|
| A1 | `core.lock.json` pins `8ba16cb`, an unmerged Core feature-branch commit; `release.tag v0.1.0` must resolve to `git_rev` | `core.lock.json` |
| A2 | `valyria-bridge-host` still refuses `session/open` on Windows (tier-3), although Core 1.9.0 ships the named pipe | IMPLEMENTATION-PLAN Phase 8 |
| A3 | Bridge doesn't expose `storage_inspect` / `storage_purge` / `memory_list`; no Search, Memory, Storage or Index surfaces | `extension/src/bridge/protocol.ts` |
| A4 | `crates/valyria-bridge/src/pty.rs` is dead code (the terminal is Code-OSS's own) | `valyria-bridge/src/lib.rs:21,34` |
| A5 | Only 2 trace fixtures; they predate `context_retrieved`, `model_server_*`, `engine_install_*`, `approval_requested.request_id` | `fixtures/traces/` |
| A6 | Live kill/adopt/resume cycle against a real Core has never run in CI; no `@vscode/test-electron` e2e; no network-namespaced offline job; no visual regression | IMPLEMENTATION-PLAN Phases 1, 9 |
| A7 | Update feed (`updateUrl`) unwired; signed/notarized installers; model-store byte-identical-upgrade gate | `docs/RELEASING.md:205`, Phase 8 |
| A8 | Working tree: modified `vscode` submodule, untracked `.vscode/`, a stray `extension/.valyria/workspace.db` (Core was run with `extension/` as its workspace) | `git status` |

---

## 1. Rules every milestone follows

1. **Core and app land together.** Any milestone that changes the wire does
   all of this in the same milestone, or it isn't done:
   - Core: types + `PROTOCOL_VERSION` minor bump + `xtask schema` +
     per-kind event schema + `event-kinds.txt` + CHANGELOG entry.
   - App: re-vendor schemas → `packages/protocol` codegen → zod decoders →
     `@valyria/state` reducer/selectors → `valyria-bridge-host` method →
     `extension/src/bridge/protocol.ts` → the surface → a re-captured trace
     fixture → tests.
   - `core.lock.json` → the **merged Core `main` SHA**, then `xtask verify-core`.
2. **Capabilities, not flags (app D6).** Every new surface declares its
   capability token, so an app build still behaves correctly against an older
   Core.
3. **No deferrals.** A milestone's exit criteria are all met, or the
   milestone stays open. "Follow-up" is not a status.
4. **Tests prove the exit criteria.** Fake-model tests are the fast gate;
   anything that claims real-model, real-OS or scale behaviour gets a CI job
   that actually exercises it (§4).
5. **Docs move with code.** Stale module docs (`orchestrator/lib.rs`,
   `sandbox/lib.rs`, `tools/search.rs`, `runtime-mlx`, `plan/validate.rs`)
   are rewritten in the milestone that invalidates them. ROADMAP.md's gap
   table shrinks every milestone and is empty at M11.
6. **One branch + one PR per milestone per repo**, merged to `main`
   (Core first, then the app PR that bumps the lock).

---

## 2. Milestones

Dependency order:

```text
M0 ──► M1 ──► M2 ──┬──► M3 ──┐
                   ├──► M4 ──┼──► M5 ──┐
                   └──► M8   │         ├──► M9 ──► M10 ──► M11
       M6 (after M1) ────────┘         │
       M7 (after M0) ──────────────────┘
```

M3/M4/M8 can run in parallel after M2; M6 only needs M1; M7 only needs M0.

---

### M0 — Baseline, hygiene, wiring lag

Get both repos onto one honest, green, merged baseline before adding anything.

**Core**
- Land C14 (no-change turn completes without verifying) with its test.
- Merge `feat/local-inference` → `main`.
- Fix `walking_skeleton.rs` flakiness (E4) at the root: find the race, don't
  add retries.
- Rewrite the stale module docs listed in §1.5 so they describe current
  behaviour.
- Replace ROADMAP.md's Known-gaps table with §0 of this document.

**App**
- A1: `core.lock.json` → merged Core `main` SHA; re-verify `release` artifacts
  resolve to it.
- A2: remove the Windows tier-3 refusal; connect over `\\.\pipe\valyria-<id>`
  (Core 1.9.0). Show Core's reported `Confinement::None` in the header and the
  Security overview until M7 lands.
- A3: bridge + `protocol.ts` + surfaces for `storage/inspect`,
  `storage/purge` (dry-run first, then confirm), `memory/list`.
- A4: delete `valyria-bridge/src/pty.rs` and its dependency.
- A5: re-capture traces from the pinned Core covering every kind in
  `event-kinds.txt`; the decoder-coverage gate reads `event-kinds.txt`
  directly rather than the fixture set.
- A8: gitignore `.valyria/` everywhere; decide and commit or revert the
  submodule change.

**Exit:** both repos' full CI green on `main`; `core.lock.json` points at
`main`; Windows opens a session over the pipe; zero stale "not implemented
yet" doc comments that describe implemented code.

---

### M1 — The real model path in the live loop  ✅ shipped 2026-09-15

The agent loop now uses what Phase 9 built: the ladder and the router.

**Core — shipped:**
- C1: `AgentDriver`, `plan_exec.rs` and `valyria-app`'s real-inference
  wiring (`runtime.rs`) all construct and drive `Arc<RoleRouter>`, not
  `Arc<Orchestrator>` — six call sites migrated (driver, both `plan_exec.rs`
  model calls, `runtime.rs`'s construction + `model_activate` +
  `model_remove` + `spawn_model_boot`, and the four agent test files'
  `build_driver` helpers). `Orchestrator` itself is kept (a smaller,
  still-tested single-binding building block; see the crate's module docs)
  but is no longer what anything live constructs.
  - `RoleRouter::generate_action` (new): walks a role's fallback chain,
    running the *full* D5 ladder (`structured::resolve_action`) against
    each candidate in turn — skipping one that's unregistered, unhealthy,
    or whose ladder attempt exhausts its reformat retries without ever
    recovering a parseable turn, the same "unreliable, not malformed"
    reasoning `generate`'s bare fallback already used for a retryable
    model error.
  - `RoleRouter` is `RwLock`-backed (mirroring `Orchestrator`'s own
    hot-swap design) — `register`/`bind`/`bind_single`/`clear` all take
    `&self`, safe to call while a generation is in flight, never holding
    the lock across an `.await`. `model_activate` / `model_remove` /
    `spawn_model_boot` rebind through it exactly as they rebound the
    orchestrator before.
  - A real, production bug fixed along the way: `plan_exec.rs`'s two model
    calls (`submit_plan`, and each plan step's implementing turn) built
    their `GenerateRequest` with **no `tools` field at all** — every
    fake-model test scripts its turns directly and never inspects what was
    offered, so this went unnoticed, but a real model had no JSON Schema
    for `submit_plan` (prose-only: *"Respond with a single `submit_plan`
    tool call"*) and no schema for anything a plan step asked it to call.
    Both now pass `.with_tools(...)` (`submit_plan_tool_spec()`, a
    hand-authored schema mirroring `valyria_plan::model::{Plan, PlanStep}`,
    and `self.tool_specs.clone()` respectively) and go through
    `generate_action`, so a real model gets both a schema to call against
    and ladder recovery if it still gets the shape wrong.
- Escalation: `AgentDriver::model_role(escalated)` picks `FastCoder` for
  the main Implementing loop and every repair attempt, unless this task's
  `SwitchRole` repair decision has already escalated it to `PrimaryCoder`
  for the rest of its run (`repair_role_primary`, now actually read — it
  used to be set and ignored) or `FastCoder` simply isn't bound (every
  install today; multi-role catalog selection is M6), in which case it
  degrades to the unconditional `Role::PrimaryCoder` every call site used
  before M1, with no behaviour change for a single-model install.
  `crates/valyria-agent/tests/repair_loop.rs::
  a_switch_role_decision_actually_escalates_from_fast_to_primary_coder`
  proves it end to end: a `FastCoder` that never converges (varying wrong
  edits, so neither `ExactRepeat` nor `Oscillation` fires early and
  `RepeatedFailure` drives the ladder as intended) hands off to a
  `PrimaryCoder` that fixes it on its first call, journaled `role`/
  `model_id` fields on `EffectCompleted{MODEL_COMPLETION}` (also new)
  naming both models by id.
- KV-cache prefix stability: `AgentDriver::build_conversation` already
  rebuilt the same policy/instructions/objective prefix every turn and
  only ever *appended* to the tool-call history — that property held
  before M1, just unexploited. `valyria-runtime-openai-compat::wire::
  build_chat_request` now sends `"cache_prompt": true` (a llama.cpp
  `llama-server` extension; harmless elsewhere — an unrecognized field on
  any other OpenAI-compatible server). A new test,
  `appending_a_message_leaves_the_earlier_json_prefix_byte_identical`,
  asserts the actual property that makes the cache hit: message N's JSON
  serialization is a strict prefix of message N+1's.

**Deliberately deferred, with reasons (not silently dropped):**
- **C2, an explicit GBNF/grammar-compilation tier** — llama-server already
  derives its own constrained-decoding grammar from a request's `tools`
  array when using its native tool-calling path. Since M1 just fixed the
  real bug (two call sites never sent `tools` at all), the practical
  benefit of also hand-compiling GBNF ourselves is materially smaller than
  it looked before that fix, and the transport ladder already recovers a
  model that ignores the grammar anyway. Revisit if the real-model bench
  (M10) shows the native mechanism under-constraining a specific model
  family.
- **`ModelPool` admission control** — moved to M6 outright (this plan's
  §0.2 already scoped its *measured-footprint* and *protocol-event*
  wiring there; M1 is the point where it stopped being premature, since
  `FastCoder` can now genuinely be bound and exercised, but the actual
  wiring — a shared `Mutex<ModelPool>` on `Runtime`, admission before
  every `spawn_model_boot`/`model_activate`, eviction driving a real
  server shutdown — is resource-lifecycle code that deserves its own
  focused pass and test suite, not a rider on this one). `ModelPool` and
  its tests are unchanged and still not constructed anywhere outside
  `valyria-orchestrator`'s own crate.
- **C9, `PlanningMode::ModelAuthored` as the default** — flipping the
  default affects essentially every existing test that doesn't pass
  `--plan` (none of them script a `submit_plan` turn), so this is a
  repo-wide test migration, not a two-line change. `ModelAuthored` stays
  opt-in; M1's actual contribution here is making it *work* against a real
  model once opted into (the tool-schema fix above).
- **Protocol 1.13 (`model_role_escalated`, `tool_call_repaired` events;
  probe metrics on `model_inspect`)** — the underlying signal already
  exists and is durable: every `MODEL_COMPLETION` journal entry (and its
  projected `model_completed` event) now carries `role` and `model_id`, so
  a client watching that field change across turns can already tell
  escalation happened without a dedicated event kind. Dedicated events and
  measured-probe metrics are real, scoped follow-up work, deferred to
  land alongside M6's pool wiring rather than half-built here.

**App** — not started this pass; the protocol additions it would consume
(dedicated events, probe metrics) are exactly what's deferred above. The
existing `model_completed` payload's new `role`/`model_id` fields are
already on the wire (no protocol version bump needed — event payloads are
documented as loose-typed, §4.27), so a future app pass can read them
immediately without a Core change.

**Exit:**
- ✅ A fake-model scenario proves escalation FastCoder → PrimaryCoder,
  driving the real driver end to end (not a unit test of the ladder or the
  ledger in isolation).
- ✅ The KV-cache prefix-stability property has a passing test.
- ✅ `cargo test --workspace` clean (1203 passed, 4 pre-existing `#[ignore]`,
  0 failed), `cargo fmt --check` and `cargo clippy --workspace --all-targets
  -D warnings` both clean.
- Deferred (see above, each with a reason): the 21-case ladder corpus
  running through the driver specifically (it already proves the ladder
  itself; a driver-level rerun is possible but not done here), the grammar
  tier, `ModelPool` wiring, planning-by-default, and protocol 1.13.

---

### M2 — Repository intelligence in the loop  🟡 partially shipped 2026-09-15

**Core — shipped:**
- C4 (part): index lifecycle at `open`, synchronous and scoped-down.
  - `valyria_app::Runtime::open` bootstraps the index (files → symbols →
    graph → embeddings) and wires the result into the driver, but **only**
    for `ModelBackend::Local`. The Fake backend — every existing CLI/agent
    test, including the timing-sensitive kill-9/resume races — keeps
    `LiveRetriever::empty()` and zero added latency, exactly as before.
  - Not shipped: this is a *blocking* bootstrap at `open` time, not the
    staged, non-blocking, resumable background bootstrap with
    `index_progress` the full design calls for. A failed embedding stage
    degrades to lexical/symbol search rather than losing retrieval, but a
    slow *index* stage still delays `open` itself. Real background staging
    is its own focused piece of work, deferred (see below).
  - Not shipped: the VFS watcher still has zero consumers (C4's
    incremental-pipeline wiring and the stale-context generation guard are
    both untouched).
- C3: `LiveRetriever` (`Static` | `Search`) replaces the hardcoded
  `StaticRetriever::empty()` in `AgentDriver::system_and_task_messages`,
  set via a new `with_retriever` builder (`Runtime::open` calls it for the
  `Local` backend). The query is the task objective (`RetrievalQuery::new`'s
  default) — not yet enriched with anchors/failing-test-locations/error
  signatures, and memory is not fused in as a second retriever (both
  deferred, see below). Every retrieval is journaled as `context_retrieved`
  with real items and their per-hit `ScoreExplanation`-derived score
  (`journal_prompt_context_retrieved`, new).
- C5/C6 (read tools, partial): `search` and `symbol_search` are real —
  the `EXCLUDED` stub list in `tool_specs.rs` no longer contains them,
  `ToolCtx` carries an optional `store: Arc<Store>` the tools open the
  fused `SearchEngine` through (mirroring `Runtime::search`'s own
  `!Send`-future/scoped-thread bridge), degrading to a clean
  `tools.search_unavailable` failure — never a panic — when no store is
  wired. `git_blame` stays excluded and stays a stub: `valyria-git` has no
  blame implementation *at all* yet (not just the tool wrapper), which is
  real, separate work this milestone didn't touch. `find_definition`,
  `find_references`, `read_many` are not built (deferred, see below).
- C7 (part): `diagnose` gets real graph neighbours. A new
  `graph_neighbors` (free function, unit-tested directly) walks
  `GraphStore::impact_of` for each changed file and feeds the
  `(changed_file, neighbor)` pairs `valyria_verify::diagnose` already knew
  how to use — a caller broken by an edit to its callee now gets flagged
  as a `GraphNeighbor` suspect instead of only being found by literal
  failure-location or change-ledger overlap. Verified end to end with a
  two-file fixture (a real caller/callee pair through a real bootstrapped
  index+graph). The verify-strategy half of C7 (changed symbols → covering
  tests, for `TargetedTest`/`RelatedTests` selection) is not built.

**Deliberately deferred, with reasons:**
- **Background, non-blocking, staged bootstrap + `index_progress` +
  the VFS watcher wiring** — real, focused infrastructure work (resumable
  staged indexing, a debounced watcher-to-incremental-pipeline bridge, the
  stale-context generation guard) that deserves its own pass rather than a
  rider on getting retrieval wired at all. The synchronous bootstrap
  shipped here is a real, working, but scoped-down first cut.
- **`find_definition` / `find_references` / `read_many` tools** — each is
  real, separate tool-surface work (symbol resolution against the index,
  a read-many batching contract) with no shared plumbing to this
  milestone's search wiring beyond `ToolCtx.store`, which they can now
  build on directly.
- **`git_blame`** — blocked on `valyria-git` having no blame
  implementation at all; a `gix`-based line-range blame is its own task.
- **LSP consumer wiring (C10)** — `valyria-lsp` still has zero consumers;
  merging its results into `SymbolResolver` ranking is real, separate
  work with its own real-server test matrix (deferred to run alongside
  M8's language-corpus expansion, where a real-server LSP matrix is
  needed anyway).
- **Real `Embedder` role (C11)** — blocked on M6's real multi-model
  wiring (an embedding-capable model actually loaded via the pool);
  `HashingEmbedder` stays the retrieval signal for now, exactly as
  before M2.
- **Query enrichment (anchors, failing-test locations, error signatures)
  and memory as a second fused retriever** — both real, bounded follow-on
  work once M3 (memory) exists to fuse in.
- **Plan target index resolution (C9 part)** — plan validation still
  resolves targets against the filesystem, not the index; unaffected by
  this milestone.
- **Protocol 1.14 and the app surfaces it would drive** (`index_progress`
  event, `IndexStatus` fields, `context_explain`, the Search panel, Index
  progress UI, a lit-up Context Inspector) — not started. `context_
  retrieved`'s existing payload already carries real items post-M2, so a
  future app pass has real data to render without a protocol change; the
  new pieces above (`index_progress`, `context_explain`) still need one.

**Exit:**
- ✅ A fake-model-independent scenario proves a real `SearchRetriever`
  finds the one relevant file among distractors and it lands in
  `context_retrieved` (`valyria-agent/tests/live_retrieval.rs`), driving
  the real `AgentDriver`.
- ✅ `search`/`symbol_search` find real content through `ToolRuntime::
  invoke` end to end (`valyria-tools/tests/search_tool.rs`).
- ✅ A caller broken by a changed callee is flagged as a graph-neighbour
  suspect, proven against a real bootstrapped index+graph
  (`valyria-agent/src/driver.rs::graph_neighbors_tests`).
- ✅ `cargo test --workspace` clean (1210 passed, 4 pre-existing
  `#[ignore]`, 0 failed), `cargo fmt --check` and `cargo clippy --workspace
  --all-targets -D warnings` both clean.
- Deferred (see above): incremental reindex / watcher observation,
  `verify_index` drift after watcher-driven updates, LSP enrichment
  against a real server, the background staged bootstrap.

---

### M3 — Memory, instructions, secret hygiene  🟡 partially shipped 2026-09-15

**Core — shipped:**
- C12 (part): repository memory extraction after a *verified* completion.
  `AgentDriver::extract_and_write_memory` (new, called from the one
  `Completed` transition in `step_verifying` that follows the mandatory
  full run actually passing — not the "nothing to verify" or "no tooling"
  honest-completion paths, which have no evidence to extract from) folds
  this task's `VerificationRunRecord`s to their last-seen outcome and
  total failure count per command, turns them into `valyria_memory::
  Observation`s (kind from the run's `Tier`), and runs them through the
  already-built (but previously never-called from the live loop)
  `valyria_memory::extract`. A command seen to pass becomes a `Command`
  memory entry; one that failed repeatedly becomes a `Pitfall`. Wired via
  a new `with_memory` builder, called from `valyria_app::Runtime::open`
  unconditionally (not gated on the model backend, unlike M2's retriever/
  store — this only ever fires on a genuine full-verification-pass path,
  which the Fake backend can reach too, and a DB write there is cheap and
  consistent to test either way). Best-effort throughout: no `memory`
  wired, no runs recorded, or a write error all just mean nothing is
  extracted — never a reason to fail an otherwise-completed task.
- C13 (part): `valyria_util::redact` — a complete, well-tested known-shape
  scanner (AWS keys, private key blocks, bearer/GitHub tokens, generic
  `KEY=value` assignments) — existed and was **never called from anywhere
  in the workspace**. It's wired now, at the one choke point every tool
  call's result passes through (`ToolRuntime::run`, right after `Tool::
  execute` returns, before the outcome becomes either a `Message::
  tool_result` or a persisted `ToolInvocationRecord`) — covering both
  context and logs the requirement is written against, not just one. Also
  extended `redact` itself with a second pass using the crate's own
  (also previously unused outside its unit tests) `looks_like_secret`
  entropy heuristic, so a bare high-entropy token with no recognizable
  prefix or `KEY=` framing is caught too, not just known shapes.
  Deliberately scoped to tool *output* — see the function's doc comment
  for why input isn't touched.

**Deliberately deferred, with reasons:**
- **Flaky-test observations, directory-convention extraction, evidence-
  triggered retirement** — `extract`'s existing heuristics cover working/
  pitfall commands only; the other observation kinds C12 calls for need
  new extraction logic this milestone didn't build.
- **The `memory_write` tool** — the model can't proactively record a
  memory entry yet; only the driver's own post-verification extraction
  writes anything. Real, bounded follow-on tool-surface work.
- **Redaction is not yet applied to context items directly** (only to
  tool output) — a secret that enters context some other way (e.g. a
  future `read_many` batching tool, or memory text itself) isn't covered
  by this pass. `valyria-context`'s candidate-assembly path is the next
  choke point, deferred.
- **Bidi/homoglyph detection, instruction re-read on watcher change** —
  unrelated code paths this milestone didn't touch; the watcher itself is
  still unwired (M2's own deferral).
- **Protocol 1.15 and the app surfaces it would drive** (`memory_list`
  without a query, `memory_add`/`delete`/`pin`, `secret_redacted`/
  `instructions_changed`/`instruction_conflict` events, the Memory panel,
  redaction notices in Activity) — not started.

**Exit:**
- ✅ A verified task writes repository memory a later retrieval finds —
  proven end to end against a real `MemoryStore`
  (`valyria-agent/tests/repair_loop.rs::
  a_verified_completion_writes_repository_memory`), not `context_
  retrieved` fusion (deferred — memory isn't wired in as a second
  retriever yet, matching M2's own deferral of that piece).
- ✅ A secret read off disk through a real tool call never reaches the
  model's context or the journal
  (`valyria-tools/tests/integration.rs::
  a_secret_read_from_a_file_is_redacted_before_it_reaches_the_model_or_the_journal`),
  and a bare high-entropy token with no known shape is caught too
  (`valyria-util/src/redact.rs`'s new tests).
- ✅ `cargo test --workspace` clean (1214 passed, 4 pre-existing
  `#[ignore]`, 0 failed), `cargo fmt --check` and `cargo clippy --workspace
  --all-targets -D warnings` both clean.
- Deferred (see above): a PEM block specifically was already covered
  pre-M3 (`private_key_block` pattern); the "asserted by scanning all
  three [prompt, log, event payload]" framing from the original write-up
  is narrower than what actually got proven (tool output + journal, not a
  full prompt/event-payload sweep) — an honest gap, not silently dropped.

---

### M4 — Tools, user interaction, git writes  🟡 partially shipped 2026-09-15

**Core — shipped:**
- The `ask_user` half of C6, done differently than planned: rather than a
  new tool, `ActionRequest::Ask` (`FinishReason::Ask`, already wired since
  before M4) already parks a task in `WAITING_FOR_USER` with the model's
  question — the actual gap was that nothing could ever answer it.
  `AgentDriver::respond_to_user(task_id, answer)` (new) journals the
  answer as a `kinds::USER_RESPONSE` entry (`Trust::Instruction` — the
  user speaking) and resumes to `Implementing`. `build_conversation` was
  rewritten from its old tool-only, `EffectId`-correlated two-pass replay
  into a single seq-ordered merge across *every* turn kind — tool call/
  result/denial (still `EffectId`-correlated) and now question/answer
  (paired by adjacency, since a model-asked question has no effect id to
  correlate against) — so the turn immediately after an answer actually
  carries both the question and the answer in the model's message
  history, proven against a real captured `GenerateRequest`, not just a
  state-machine assertion. `valyria_app::Runtime::respond_to_user` mirrors
  `resolve_permission_scoped`'s wrapper shape exactly (spawn a fresh
  driver run if the answer leaves the task live).
- `git_commit`, the first of C6's git-write tools. `valyria-git` has no
  write API at all (confirmed, not assumed), so this shells to the real
  `git` binary through the same sandboxed-process path `run_command`
  already uses (`ctx.launcher.wrap` + `valyria_process::run`) rather than
  building gix-based write support from scratch — `git add <paths|-A>`
  then `git commit -m <message>`, both risk-classified through the
  existing `classify_command` (so a genuinely dangerous invocation still
  gets caught by the same mechanism every shell command does), under
  `PermissionCategory::Filesystem`/`ActionKind::Write` (a plain commit is
  not `GitHistoryModification` — that category, denied by default, is
  for force-push/hard-reset/rebase/filter-branch, none of which this tool
  can invoke). Proven against a real repository: the test asserts the
  actual `git log`/`git status`/`git show` afterward, not just that the
  tool reported success.
- **A real sandbox bug found and fixed along the way, not scoped work**:
  writing `git_commit`'s test revealed that the macOS Seatbelt profile
  denied writes to `/dev/null` — `git`, like most well-behaved CLI tools,
  opens it unconditionally as part of ordinary operation, and with no
  workspace-write-scope covering it, the "no explicit allowance" default
  denied it. This wasn't about `/dev/null`'s safety (there is nothing to
  leak or persist by writing to a discard sink); it was `render_profile`
  literally having no rule for it. Fixed: `/dev/null`, `/dev/zero`,
  `/dev/random`, `/dev/urandom` are now always readable/writable
  regardless of the configured write scope. A dedicated end-to-end
  sandbox test (`end_to_end_dev_null_is_always_writable`) guards it
  directly, independent of `git_commit`.
- A latent bug in the test suite's own `expect_success` helper (`valyria-
  tools/tests/integration.rs`) surfaced by the same investigation: it
  asserted internal *consistency* between `outcome.is_success()` and
  `record.success` but never that the outcome was actually a success —
  so a tool call that failed *consistently* (both fields agreeing it
  failed) passed straight through a helper whose name promised the
  opposite. Fixed with one added assertion; every existing caller was
  already only ever feeding it genuine successes, so this changed no
  other test's outcome.

**Deliberately deferred, with reasons:**
- **`apply_patch`, `git_branch`, `git_stash`, `git_checkout`,
  `report_finding`, `plan_update`** — each real, separate tool-surface
  work; `git_commit` was the one git-write tool built and proven this
  pass, not all four from the original write-up.
- **Structured `task_create` attachments** — the "preamble text" path
  this would replace is untouched; a real, separate feature.
- **Protocol 1.16 and the app surfaces it would drive** (`task_respond`
  as a wire method — the mechanism exists in Core now, but nothing
  exposes it over the protocol yet — `TaskCreateRequest.attachments`,
  `user_question` event, `permission_rules`, the app's reply composer,
  attachment UI, and permission-rules page) — not started.

**Exit:**
- ✅ A fake-model scenario asks a question, `respond_to_user` answers it,
  and the *next model call's actual message history* — not just the
  journal or the state machine — carries both
  (`valyria-agent/tests/conversation_history.rs::
  respond_to_user_answers_the_question_and_the_next_turn_sees_both`).
- ✅ `git_commit` produces a real commit a real `git log`/`show` confirms,
  and a nothing-to-commit call fails cleanly rather than silently
  succeeding (`valyria-tools/tests/integration.rs`).
- Deferred (see above): approval-gated commits in Assisted mode specifically
  (the general permission-mode machinery already governs every write tool
  including this one, but no dedicated test targets `git_commit`
  specifically the way the write-up describes); `git push --force`/
  `reset --hard` refusal (no tool can invoke them at all yet — `git_branch`/
  `stash`/`checkout` don't exist, and `git_commit` has no path to them);
  attachments in `context_retrieved`.
- ✅ `cargo test --workspace` clean (1218 passed, 4 pre-existing
  `#[ignore]`, 0 failed), `cargo fmt --check` and `cargo clippy --workspace
  --all-targets -D warnings` both clean.

---

### M5 — Planning and multi-agent  🟡 partially shipped 2026-09-15

**Shipped**

- Child tasks (`valyria-task`): `TaskManager::create_child` links a new
  task's `parent_task` back to its creator and inherits `workspace_id`
  from it — the schema/struct field already existed (Phase 8) but had no
  writer. `children_of` is the read side (direct children only, oldest
  first — not recursive; a grandchild belongs to its own parent). A child
  gets a fully independent journal/budget, same as any task — nothing
  about crash recovery, resume, or `count_model_calls` needed to change,
  since those were already scoped per-`TaskId`.
- Cascading pause/cancel: `request_pause`/`request_cancel` now walk every
  descendant (recursively, via `cascade_signal_to_children`) and write the
  same durable `pending_signal` onto each active one — a parent pause with
  no cascade would leave children running unsupervised with no driver
  left checking on them. Terminal descendants are skipped (nothing will
  ever read their `pending_signal` again).
- C8 — verify state reconstructed from the journal on resume: `DIAGNOSIS`
  journal entries now carry `file_state_hash`/`verification_frontier`/
  `failure_count`/`files_touched` (previously only used in-process, never
  persisted), and a new `REPAIR_ATTEMPT` journal entry kind records each
  `RepairAttempt` before it's folded into the (process-local)
  `RepairLedger`. `AgentDriver::take_verify_state` is now `async` and, the
  first time a fresh process touches a task's verify state, replays that
  task's entire journal through `reconstruct_verify_state` to rebuild the
  `LoopDetector`'s history and the `RepairLedger`'s attempt count/
  escalation flags exactly as they'd stand had the process never
  restarted — the same technique `plan_exec::plan_rejection_count` already
  used for the plan-repair budget. Proven with both a unit-level replay
  suite (`driver::reconstruct_verify_state_tests`, 6 tests) and a real
  crash-and-resume integration test
  (`a_crash_mid_repair_does_not_reset_the_attempt_budget_or_loop_history`)
  that aborts a live driver mid-repair-loop, rebuilds a completely fresh
  `AgentDriver`, and asserts the total repair attempts across both
  processes still respect the same budget a single uninterrupted process
  is held to.
- Role pipeline wiring (`valyria-agent::role_pipeline`): `AgentDriver::
  run_role_pipeline` runs Researcher → Planner → Implementer → Tester →
  Reviewer as a real sequence of child tasks under one coordinator,
  persisting each role's typed `Artifact` (`ResearchBrief`/`Plan`/
  `ChangeSet`/`VerificationReport`/`ReviewFindings`) to `PlanStore` — the
  only channel roles communicate through, exactly as `valyria-plan`'s
  Phase-8 design called for. Reuses existing machinery wherever a role's
  job actually matches it rather than reinventing anything: Planner calls
  `step_planning` verbatim (the exact "ask for `submit_plan`, validate,
  repair under budget" cycle single-task `Planning` already does);
  Implementer reuses the *entire* single-task state machine on its own
  child, pre-loaded with the Planner's accepted revision, so it gets the
  full edit/verify/diagnose/repair loop for free; Tester makes no model
  call at all — its report is read straight from the `VerificationLog`
  rows the Implementer's own mandatory verification already produced;
  Researcher and Reviewer (read-only, no existing analogue) get a new
  bounded Reason/Select/Execute loop restricted to their tool allowlist
  plus a role-specific "submit" tool.

  This surfaced two real, previously-untested gaps, both fixed:
  - **Model routing was role-blind.** `AgentDriver::model_role` only ever
    resolves to `FastCoder`/`PrimaryCoder` — nothing in the live loop ever
    used the `Planner`/`Reviewer` `ModelRole`s valyria-model-registry has
    defined since Phase 8. A role pipeline genuinely needs each role
    routable to its own model (so Researcher/Reviewer can use a cheap
    model while Implementer uses the strong one), and — as a test
    surfaced immediately — the *existing* single-task `Planning`/
    `Implementing` model calls have no way to be pinned to a specific
    role independent of the global FastCoder-bound-or-not check, which
    would otherwise make an unrelated role's binding hijack another
    role's model choice. Fixed additively: `step_planning`/
    `request_plan_from_model`/`step_implementing_plan`/
    `request_step_action` all now take an `Option<Role>` override
    (`None` — every pre-M5 call site via `run` — preserves prior behavior
    bit-for-bit); `AgentDriver::run` is now a thin wrapper around a new
    `pub(crate) run_with_role`. `role_pipeline::preferred_model_role`
    maps each `AgentRole` to its `ModelRole` (Researcher→FastCoder,
    Planner→Planner, Implementer→PrimaryCoder, Reviewer→Reviewer),
    falling back to the ordinary `model_role` selection when nothing
    role-specific is bound, so a driver that only ever bound
    `PrimaryCoder` (every pre-M5 setup) still runs a role pipeline
    end-to-end on that one model.
  - **`Timestamp` could not survive `serde_json` at all.** `Artifact::Plan`
    embeds a `PlanRevision`, which embeds a `Timestamp` (`u128` newtype);
    `PlanStore::save_artifact` serializes the whole `Artifact` via
    `serde_json::to_string`, which errored unconditionally
    ("u128 is not supported" — serde_json has no native u128 support
    without the `arbitrary_precision` feature, which this workspace
    doesn't enable). This was real, latent, and pre-existing: nothing had
    ever tried to serialize an `Artifact::Plan` before (the one existing
    `Artifact` serde test in `valyria-plan` used `VerificationReport`),
    and every other `Timestamp` persistence path in the codebase
    deliberately goes through a raw SQL column, not `serde`. Fixed at the
    source (`valyria-types::Timestamp`, layer 0): serializes as a decimal
    string instead of a bare number — full range, no precision loss, and
    since serialization always errored before, nothing could have been
    depending on a numeric JSON shape. New regression tests
    (`time::tests::round_trips_through_serde_json`,
    `rejects_a_non_numeric_string`) guard it directly.

  Proven end-to-end: `role_pipeline.rs`'s
  `the_full_role_pipeline_runs_end_to_end_and_persists_every_artifact`
  binds four *independently scripted* fake models (one per role — proving
  routing is real, not coincidental sharing), drives a coordinator task
  through the whole pipeline, and asserts the real file edit happened,
  all five artifacts were persisted with the right role/kind, and all
  four spawned children are linked back via `parent_task` and reached a
  terminal state. A second test,
  `a_reviewer_rejection_hands_the_coordinator_to_waiting_for_user_not_completed`,
  proves a Reviewer that flags a problem blocks completion rather than
  being silently ignored.

**Deliberately deferred, with reasons**

- Role-pipeline coordinator crash-recovery: a coordinator killed
  mid-pipeline restarts `run_role_pipeline` from scratch on the next call,
  which would mint duplicate child tasks rather than resuming the ones
  already in flight — every *child* task itself is fully crash-safe (an
  ordinary task, recovered exactly like any other), so no work is
  silently lost, but the coordinator's own "which role am I on"
  bookkeeping needs a follow-up (checking `children_of(task_id)` before
  spawning a new child) to make resuming the pipeline itself idempotent.
- Auto-repair-revision loop: a Reviewer finding or a failing Tester
  report hands the coordinator to `WaitingForUser` rather than looping
  back into a new Implementer revision that addresses the feedback — real
  follow-up work, not a corner cut silently (see the test proving the
  hand-off happens rather than being ignored).
- Role-pinned model routing only covers the Planner/Implementer's *first*
  pass through `Planning`/`Implementing`; if the role-pipeline
  Implementer's own mandatory verification fails and it enters
  `Diagnosing`/`Repairing`, that repair cycle's model call falls back to
  the ordinary global `model_role` selection (not the role-pinned one) —
  immaterial for a passing implementation, but a real gap if a role
  pipeline's Implementer needs to self-repair while a differently-bound
  role (e.g. Researcher's `FastCoder`) is also active.
- Per-step verification scoped to one wave (rather than the full run
  before `Completed`): the mandatory full `Verifying` suite after the
  whole plan remains the only verification pass — a parallel-wave child
  deliberately never reaches its own `Verifying` (see below).
- App surfaces for the protocol below (task tree, plan DAG with lanes,
  artifact viewer, plan-revision diff, per-child pause/cancel): the Core
  wire types and their dispatch are shipped (see below) and reachable
  from the Rust `EmbeddedClient`, but `valyria-app`'s TypeScript side —
  `valyria-bridge`'s Rust wrapper methods, `valyria-bridge-host`'s
  JSON-RPC dispatch, the `Requests` interface in
  `extension/src/bridge/protocol.ts`, `@valyria/protocol`'s vendored
  schemas + Zod decoder registry, `@valyria/state`'s reducer/selectors for
  the two new subtask events, and any actual webview UI — is genuinely
  separate, substantial frontend work across a second repository, not yet
  started. Tracked as the next M5 chunk.
- Wave-crash mid-resume: a parallel batch killed partway through leaves
  whichever children hadn't finished stuck (their parent never folds a
  non-terminal child's result back in, so the parent itself stays
  `Implementing` rather than silently marking the wave done) — safe, but
  resuming doesn't yet re-drive just the unfinished children specifically;
  the whole `step_implementing_plan` call re-enters and re-derives the
  group's incomplete set from scratch, which is correct but not proven
  under an actual `kill -9` the way the single-task walking-skeleton tests
  prove sequential resume.

**Shipped (continued)**

- Parallel wave executor (`valyria-agent::plan_exec`): a wave's
  `parallelizable` steps — the `Schedule::waves()`/`Group` grouping
  `valyria-plan` already computed and tested — now actually run as
  concurrent child tasks instead of the flattened sequential `order()`.
  `Schedule::group_for` (new) finds which group the next incomplete step
  belongs to; a group with more than one still-incomplete member hands
  off to `run_parallel_group`, which partitions it into target-conflict-
  free batches (`partition_conflict_free` — steps whose declared `targets`
  overlap are never scheduled concurrently, computed statically from the
  plan's own declarations before anything runs, not discovered after the
  fact from the ledger) capped at `MAX_PARALLEL_CHILDREN` (4) concurrent
  children per batch, runs each batch via `futures::future::join_all`, and
  folds every success back into the parent's own journal
  (`PLAN_STEP_STARTED`/`PLAN_STEP_COMPLETED`, a checkpoint if declared) so
  the ordinary schedule-driven loop treats them exactly like sequential
  steps on its next iteration. Any child that doesn't cleanly complete
  (needs permission, asks the user, or fails) fails the whole wave rather
  than attempting a partial resume — a bounded, honest limitation, not a
  silent gap.

  `AgentDriver::task_changed_files` is now recursive over a task's
  children (a parallel step's edits are ledger-recorded under its own
  child `task_id`, invisible to the parent's own entries otherwise) —
  needed for checkpoints, loop-detector file-state hashing and Diagnosing
  suspects to see what a wave's children actually touched.

  This surfaced two more real bugs, both fixed:
  - **The same role-blind-routing hazard from role-pipeline wiring,
    recurring in the *ordinary* (non-role-pipeline) single-task path.**
    `model_role`'s "prefer `FastCoder` whenever it's bound" check is
    global and purpose-blind: the moment a driver binds `FastCoder` for
    *any* reason (parallel-wave children benefit from a cheaper model
    since there are many of them — `run_parallel_step_child` prefers it
    when bound), every *other*, unrelated task's ordinary `Planning`/
    `Implementing` calls silently get redirected to it too, with no way
    to opt out. Fixed by making `AgentDriver::run_with_role` — already
    added for role-pipeline wiring — `pub` instead of `pub(crate)`, so a
    caller assembling a driver with several such bindings can pin an
    ordinary top-level task's own role explicitly rather than trusting
    `model_role`'s ambient, whole-process-wide guess.
  - **Parallel-wave children never reached a terminal state.** The first
    implementation returned as soon as a child's one step was marked
    complete, leaving the child parked at `Implementing` forever —
    indistinguishable from a task a crash actually interrupted, and
    exactly the class of bug `role_pipeline::finish_role_child` already
    exists to prevent for role children. Fixed the same way: walk the
    child through `Verifying` → `Completed` as pure bookkeeping
    (`transition` has no side effects beyond journaling; the real
    verification command only ever runs inside `step_verifying`'s own
    handler, which this never calls) once its step is done.

  Proven in `plan_loop.rs`: `parallel_steps_in_one_wave_run_as_concurrent_
  child_tasks` (two `parallelizable` steps with disjoint targets spawn two
  children, both linked to the parent via `parent_task` and terminal, both
  steps recorded complete on the parent, exactly one plan revision — no
  spurious re-planning) and `a_parallel_group_with_overlapping_targets_
  still_completes_via_serialized_batches` (two steps sharing a target
  still both complete, proving `run_parallel_group` correctly walks
  multiple batches rather than assuming one always covers a whole group).
  `partition_conflict_free` itself has a direct unit-test suite (4 tests)
  covering disjoint targets landing in one batch, an overlap forcing a
  separate one, a step joining the first non-conflicting batch rather than
  always opening a new one, and read-only (no-target) steps never
  conflicting. Proving genuine *wall-clock* concurrency (the original
  spec's "timestamps overlap") isn't practical with the fake-model test
  harness, which has no controllable per-call delay primitive without
  risking flakiness — these tests instead prove functional and structural
  correctness of the orchestration, which is where the real, new risk
  was (and where both bugs above were actually found).

- Protocol 1.13.0 — the Core (Rust) half of what the original plan called
  "Protocol 1.17" (§3's evolution table used the plan's own milestone
  numbering; the actually-enforced `PROTOCOL_VERSION` constant follows
  its own minor-bump rule and lands at 1.13.0). `valyria-events::
  EventKind` gains three variants (`SubtaskStarted`, `SubtaskCompleted`,
  `ArtifactPublished`), each with a pinned `event_payloads` struct and a
  `docs/protocol/events/*.schema.json` entry. `TaskManager::create_child`
  now journals `subtask_started` onto the *parent's* journal (not the
  child's own — a client watching the parent is who needs to learn a
  child started), and `TaskManager::transition` journals
  `subtask_completed` onto the parent whenever a child reaches a terminal
  state; `role_pipeline::save_role_artifact` journals `artifact_published`
  after every `PlanStore::save_artifact` (`PlanStore` itself has no
  `EventBus` — layer 5, no event-projection concept — so this goes
  through the same journal→`project_events` path every other agent-driven
  event does, not a parallel emission mechanism). `TaskSummary` gains an
  additive `parent_task_id: Option<String>`. Three new request/response
  pairs — `task_children`, `task_artifacts`, `plan_revisions` (the last
  carrying each revision's structural diff against the one before it,
  via `Plan::diff`, already built in Phase 8) — are wired all the way
  through `valyria-app`'s `Runtime` (three new thin wrapper methods) and
  `EmbeddedClient`'s dispatch (three new match arms + wire-mapping
  helpers), so they're callable today through the in-process client and
  the daemon transport alike (`SocketClient` is a pure backend swap over
  the same `Request`/`Response` types, unchanged). New capability token
  `multi_agent`. `cargo xtask schema` regenerated `docs/protocol/`;
  `cargo xtask check-protocol` and `check-layering` both pass.

  Proven with protocol round-trip tests (`envelope::tests::multi_agent_
  request_and_response_variants_round_trip`), two new `valyria-task`
  tests proving the parent/not-the-child fan-out precisely
  (`subtask_events_project_onto_the_parents_own_stream`,
  `a_top_level_task_never_fires_subtask_events`), and a new assertion in
  the role-pipeline end-to-end test confirming all five artifacts each
  fire their own `artifact_published` event with the right `kind`.

**Exit (partial):**
- ✅ A loop detected before a crash is still counted after resume (proven
  end-to-end, not just at the unit level).
- ✅ A repair-ledger budget exhausted across a crash does not reset.
- ✅ The role pipeline runs end to end: Researcher, Planner, Implementer,
  Tester and Reviewer each produce and persist their typed artifact, each
  routed to its own model, with a real file edit landing on disk.
- ✅ A Reviewer finding blocks completion (hands off to a human) rather
  than being silently ignored.
- ✅ Two parallel steps in one wave run as concurrent child tasks, both
  linked back to the parent and both completing correctly.
- ✅ Overlapping targets serialize into separate batches rather than
  racing.
- ✅ `task_children`/`task_artifacts`/`plan_revisions` are real, callable
  requests, server-side, over both transports.
- Deferred to the next M5 chunk: the `valyria-app` TypeScript/bridge/
  webview surface for the protocol above; `kill -9` mid-wave resuming
  just the unfinished children (rather than being merely safe); a
  Reviewer finding causing an automatic repair *revision* (today it hands
  off to a human instead); role-pipeline coordinator crash-recovery.

---

### M6 — Model platform, complete

**Core**
- M1: `valyria-runtime-mlx`, a managed `mlx_lm.server`.
  - Engine store provisions an isolated Python env under
    `~/.valyria/engines/mlx/` (pinned versions, hash-verified wheels).
  - Strict handshake + health; reuses `OpenAiCompatRuntime` for the wire.
  - Catalog carries MLX variants; hardware selection prefers MLX on Apple
    silicon when the probe shows it faster.
- M2: engine variants (Metal, CUDA, ROCm, Vulkan, CPU) chosen from
  `valyria-hardware`, verified by the probe, with fallback to CPU recorded,
  not silent.
- M3: `model_endpoint_add { url, api_key_ref? }` for existing local servers.
  - The policy floor allows loopback/unix only unless network policy permits.
  - Endpoints are probed with the same ladder and bindable to roles.
- M4: signed catalog refresh (ed25519, key compiled in) with embedded
  fallback; a refreshed catalog never downgrades a pinned hash.
- M5:
  - Pool admission uses the probe's measured RSS.
  - `ResourcePressure` / `Evicted` / `Loaded` are projected as protocol
    events.
  - 16 GB unified-memory target: coder + embedder coexist (asserted with the
    pool's budget model).
- Role bindings auto-derived for every role from installed models via
  `RoleBinding::derive`, with user overrides in `global.db`.

**Protocol 1.18** — `model_endpoint_add/remove/list`, `catalog_refresh`,
`pool_status`; `model_pool_*` events; `ModelSummaryWire.backend`.

**App** (keeping the product decision from app commit `422be82`: no per-role
picker in the main flow)
- Models panel shows backend (llama.cpp Metal/CUDA/…, MLX, endpoint) and a
  pool memory meter.
- Endpoints and role overrides live under Settings → Models (advanced).
- Catalog refresh button.

**Exit:**
- The seeded-bug suite completes on **three adapters** (llama.cpp, MLX,
  openai-compat endpoint) on real hardware in the nightly real-model job
  (PLAN §6 criterion 4).
- Forced memory pressure evicts the embedder and not the coder, visible in
  the app.

---

### M7 — Sandboxing and security, complete

**Core**
- P1 Linux:
  - Landlock (fs), seccomp-bpf (syscall denylist + network deny per profile),
    user + mount + net namespaces, cgroup v2 memory/CPU/pids caps.
  - The level is detected from kernel features, degrades per mechanism, and
    reports exactly which mechanisms are active.
- P1 Windows:
  - Job Objects (kill-on-close, memory/process caps), restricted token,
    low-integrity level, AppContainer where available.
  - Confinement reported as `Partial` with specifics.
- macOS: profile generation audit (network deny unless profile allows, no
  writes outside workspace + tmp).
- P2: `doctor` runs a live self-test — attempts a write outside the
  workspace, a network connect and a fork bomb under the active launcher —
  and reports the achieved level.
- P3:
  - Escape-attempt corpus per OS as a CI gate (path traversal, symlink
    escape, `/proc` tricks, env credential reads, network exfil).
  - Malicious-repo corpus: git hooks, `.gitattributes` filters, repo-provided
    binaries, symlink bombs, archive bombs.
  - `docs/SECURITY.md` threat model with explicit non-goals.
- Startup `sandbox_confinement` event.

**Protocol 1.19** — `sandbox_confinement` event; `DoctorCheck.self_test`
details.

**App** — the Security overview and header render the achieved confinement
per mechanism from the event. The remaining Windows-specific copy is replaced
by the real reported level. `docs/SECURITY-REVIEW.md` is updated.

**Exit:** the escape corpus fails closed on macOS, Linux and Windows CI
runners; the malicious-repo corpus executes nothing; `doctor` self-test
matches the corpus result on each OS.

---

### M8 — Languages, complete

**Core**
- L1 tier-1 additions (queries + provider + extraction corpus each):
  C, C++, C#, Ruby, PHP, Kotlin, Swift.
- L1 tier-2 structure-only: Scala, Elixir, Zig, Lua, Bash, SQL, HCL, YAML,
  TOML, JSON, Markdown.
- L2 verify discovery for CMake / Make-C, `*.csproj` / `*.sln`, Gemfile /
  Rakefile, `composer.json`, Gradle (Groovy + KTS), `Package.swift`. Parsers
  for gcc/clang, MSBuild/dotnet test, RSpec/Minitest, PHPUnit, Gradle/JUnit
  XML, `swift test`.
- LSP configs for clangd, csharp-ls/OmniSharp, ruby-lsp, intelephense,
  kotlin-language-server, sourcekit-lsp.
- `full` cargo feature = every grammar; release binaries build `full`.

**App** — no protocol change; search, symbols and diagnostics light up per
language. Language chips in Search reflect `IndexStatus` languages.

**Exit:** the extraction corpus passes for all 11 tier-1 languages; discovery
finds the right commands on a fixture repo per ecosystem; the parser corpus
covers every new tool's real captured output.

---

### M9 — Interface, complete

**Core**
- I1: multi-workspace daemon.
  - One user-level `valyria serve` holds N `Runtime`s.
  - `workspace_open` / `workspace_close` / `workspace_list`; every request and
    subscription carries `workspace_id`.
  - Idle workspaces are unloaded; per-workspace auth scoping.
- I2:
  - Length-prefixed framing negotiated in `hello` (newline stays default for
    compatibility).
  - `xtask schema --ts` emits TypeScript declarations checked into
    `docs/protocol/ts/` and gated like the JSON Schemas.
- I3 remainder: `context_explain` (M2), `bench_run` (async, progress events).
- I4: CLI `search` (with `--explain`), `index {status,build,verify}`,
  `review` (Reviewer role over the working diff), `fix` (objective from the
  last failing check), `explain` (read-only Researcher), `benchmark` (via
  `bench_run`; the CLI still links no agent crate — D11 layering stays green).
- TUI parity: plan DAG, child tasks, approvals with scope, `ask_user`
  replies, models/pool.

**Protocol 1.20** — `workspace_open/close/list`, `workspace_id` on requests,
`framing` negotiation, `bench_run` + `bench_*` events.

**App**
- The supervisor adopts the single user-level daemon when `multi_workspace`
  is advertised (per-workspace daemons kept as the fallback path for older
  Cores).
- Multi-root VS Code workspaces map to `workspace_open` per folder, with one
  store keyed by `workspace_id`.
- `packages/protocol` generates from Core's TS export instead of its own
  JSON-Schema codegen.
- Benchmark runner view.

**Exit:**
- A multi-root window drives two workspaces through one daemon, and killing
  the window adopts both.
- A deliberate TS drift fails Core CI and app CI.
- Every PLAN §4.28 CLI command exists with `--json`.

---

### M10 — Evaluation and performance at scale

**Core**
- E1: nightly self-hosted real-model job runs the full bench suite on
  llama.cpp + MLX. `acceptance.rs` has **zero** deferrals.
- E2:
  - `xtask corpus fetch` pulls pinned real repos (by commit) into a cache,
    never vendored.
  - criterion benches for every PLAN §9 budget, tracked nightly with
    regression alerts.
  - `verify-index` fuzz at 10k operations incl. branch switches.
- E3:
  - 30-repo discovery corpus.
  - Captured-output parser corpus (recorded from real tool runs).
  - SWE-bench Lite/Verified adapter (containerized task envs, opt-in job).
  - `cargo-fuzz` targets (patch, diff, protocol decoder, tool inputs, path
    resolution) nightly with a corpus in the repo.
  - LSP matrix against real servers.
- Perf work driven by the numbers: whatever misses §9 gets fixed here, not
  re-labelled.

**App** — perf budgets (app PLAN §9) measured against a real Core under the
bench load: 50k-event stream, 100k-file workspace, 10k-line diff.

**Exit:**
- Every PLAN §9 budget is met and recorded in `docs/BENCHMARKS.md` from a
  real run.
- Every PLAN §6 criterion is demonstrated with a real local model offline.
- Nightly fuzz runs 24h without a crash.

---

### M11 — App completion and 1.0 release

**App**
- A6:
  - Integration suite in CI against the real Core sidecar + fake model:
    create → kill window → adopt → resume with zero missed/duplicated `seq`;
    kill daemon → restart → rehydrate.
  - Approval flows, rollback, multi-workspace.
  - `@vscode/test-electron` e2e over the packaged app: first run → install
    model → open repo → task → approve → review diff → roll back.
  - Network-namespaced offline job over the whole integration suite.
  - Visual regression of every webview in light/dark/HC on three OSes.
- A7:
  - Update feed wired (`updateUrl` + signed release feed).
  - Model-store byte-identical-across-upgrade gate (two real builds).
  - Signed/notarized installers on three OSes.
  - Open VSX publish.
- Manual VoiceOver + NVDA pass and keyboard-only full-task traversal,
  recorded in `SECURITY-REVIEW.md` / `ACCESSIBILITY.md`.

**Core** — `v1.0.0`: CHANGELOG cut; ROADMAP.md retired in favour of a
STATUS section with no open gaps; release pipeline publishes the binaries
`core.lock.json.release` pins.

**Exit:** app `1.0.0` and Core `1.0.0` released, signed, from CI; every gate
in both repos green; both acceptance tables fully demonstrated.

---

## 3. Protocol evolution summary

| Version | Milestone | Additions |
|---|---|---|
| 1.13 | M1 | `model_role_escalated`, `tool_call_repaired`; probe metrics |
| 1.14 | M2 | `index_progress`; richer `IndexStatus`; `context_explain` |
| 1.15 | M3 | `memory_add/delete/pin`, browse-all `memory_list`; `secret_redacted`, `instructions_changed`, `instruction_conflict` |
| 1.16 | M4 | `task_respond`, `task_create.attachments`, `permission_rules`; `user_question` |
| 1.17 | M5 | `task_children`, `task_artifacts`, `plan_revisions`; subtask/artifact events |
| 1.18 | M6 | `model_endpoint_*`, `catalog_refresh`, `pool_status`; pool events |
| 1.19 | M7 | `sandbox_confinement`; doctor self-test detail |
| 1.20 | M9 | multi-workspace ops, framing negotiation, `bench_run` |

All additive (minor bumps, capability-gated). Numbers shift if milestones run
in parallel; the gate is "bump on every wire change", not these exact numbers.

## 4. CI additions

| Repo | Job | Milestone |
|---|---|---|
| Core | `lsp-real` (rust-analyzer, gopls, pyright, tsserver, clangd) | M2, M8 |
| Core | `sandbox-escape` × macOS / Linux / Windows | M7 |
| Core | `real-model-nightly` (self-hosted, Apple silicon + CUDA) | M6, M10 |
| Core | `perf-nightly` (criterion + corpus) | M10 |
| Core | `fuzz-nightly` (`cargo-fuzz`) | M10 |
| Core | `ts-schema` drift gate | M9 |
| App | `core-integration` (real sidecar, fake model) | M11 (started M0) |
| App | `e2e-electron`, `offline-netns`, `visual-regression` × 3 OS | M11 |
| App | `installers-signed` × 3 OS | M11 |

## 5. Decisions and inputs needed from you

Defaults are chosen so work can proceed.

1. **Merge `feat/local-inference` to `main` in M0.** Default: yes.
2. **Roles UX.** App commit `422be82` removed the per-role picker. Default:
   Core auto-binds every role; the app shows bindings read-only on the Models
   panel with overrides under Settings → Models.
3. **App daemon topology after M9.** Default: one user-level multi-workspace
   daemon when Core advertises it; per-workspace daemons remain the fallback.
4. **SWE-bench adapter needs containers.** Default: opt-in nightly job, not
   part of the offline gate.
5. **External endpoints (M6).** Default: loopback/unix sockets only unless the
   network policy explicitly allows a host.
6. **Infrastructure only you can provide:** Apple/Windows signing
   credentials, a self-hosted Apple-silicon runner and a CUDA runner for the
   real-model nightly, an ed25519 catalog signing key, and the manual
   screen-reader passes.
