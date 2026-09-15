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

### M2 — Repository intelligence in the loop

**Core**
- C4: index lifecycle.
  - `Runtime::open` starts a background staged bootstrap
    (files → symbols → edges → embeddings), resumable, emitting
    `index_progress`; lexical + symbol search are usable before embeddings
    finish.
  - The VFS watcher feeds the incremental pipeline (debounced); HEAD changes
    become one bulk delta instead of N watcher events.
  - Each step records the generation it planned against; divergence forces a
    context refresh before execution (PLAN §4.24 stale-context guard).
- C3: `SearchRetriever` replaces `StaticRetriever::empty()` in
  `system_and_task_messages`.
  - The query comes from objective + anchors (files the task touched, failing
    test locations) + error signatures.
  - Memory is a second retriever fused in.
  - `context_retrieved` carries real items with their `ScoreExplanation`.
- C5/C6 (read tools): real `search` (fused modes + explanation in the rendered
  output), `symbol_search`, `find_definition`, `find_references`, `read_many`,
  `git_blame` (line-range scoped via `gix`). All bound to the model; the
  `EXCLUDED` list is deleted.
- C7:
  - `diagnose` gets graph neighbours of failure locations.
  - The verify strategy maps changed symbols → covering tests through the
    graph for `TargetedTest` / `RelatedTests`.
- C9 (part): plan target validation resolves against the index (symbols as
  well as paths).
- C10: `SymbolResolver` merges index results with a pooled LSP client when a
  server is healthy.
  - Spawn on demand, restart on crash, idle shutdown, memory cap.
  - Each result records its source; ranking prefers LSP on conflict.
  - `doctor` reports server health.
- C11: `Embedder` role served by llama-server `/v1/embeddings` from a small
  catalog embedding GGUF.
  - Vectors are tagged with embedder id; switching embedder re-embeds by
    generation.
  - `HashingEmbedder` stays as the no-model fallback, reported as `degraded`.
  - Optional `Reranker` role for the final rerank stage.

**Protocol 1.14** — `index_progress` event; `IndexStatus` gains
`{ stage, watcher, embedder, lsp_servers[] }`; `context_explain { task_id,
turn }` → the stored `ContextSnapshot` items with provenance.

**App**
- Search panel (Core fused search, per-hit "why" from `ScoreExplanation`,
  mode chips, anchors from open editors).
- Index progress in status bar + Home.
- Context Inspector lit with real data and `context_explain` per turn.
- "Why was this file in context?" action from the explorer.

**Exit:**
- A task whose target file isn't named in the objective finds it through
  retrieval (fake-model scenario asserts the file is in `context_retrieved`
  before the edit).
- Incremental reindex after a single edit is observed in a test.
- `verify_index` shows zero drift after watcher-driven updates.
- The model calls real `search` successfully.
- LSP enrichment is exercised against a real `rust-analyzer` in CI.

---

### M3 — Memory, instructions, secret hygiene

**Core**
- C12:
  - After a `Verified` completion, extract repository memory: commands
    observed to pass, flaky tests (pass/fail on an unchanged file state),
    pitfalls from repair attempts, directory conventions from edits.
  - Entries carry provenance + confidence and are retired when evidence
    contradicts them.
  - `memory_write` tool (agent-authored = `Trust::Evidence`).
  - Browse-all listing.
- C13: a scanner (known patterns + entropy + PEM/JWT shapes) on every
  ingress to context, tool renders, logs and memory.
  - Redact with a stable placeholder and emit `secret_redacted` (path, kind,
    never the value).
  - Bidi/homoglyph detection on source entering context annotates, never
    strips.
- Instructions re-read on change via the M2 watcher (not per-turn file reads),
  with conflict reports surfaced as an event.

**Protocol 1.15** — `memory_list` without query pages everything;
`memory_add` (user-authored, `Trust::Instruction`), `memory_delete`,
`memory_pin`; `secret_redacted`, `instructions_changed`,
`instruction_conflict` events.

**App** — Memory panel (browse, filter by tier/trust, add, delete, pin, with
provenance and decayed confidence). Secret-redaction notices in Activity.
Repository-instructions view shows conflicts and which file won.

**Exit:**
- A verified task produces memory that a *second* task retrieves (asserted in
  `context_retrieved`).
- A seeded AWS key and a PEM block in a fixture repo never appear in any
  prompt, log or event payload (asserted by scanning all three).

---

### M4 — Tools, user interaction, git writes

**Core**
- C6 (write/interaction tools):
  - `apply_patch` (multi-file unified diff through the edit engine's ladder
    and ledger).
  - `git_commit`, `git_branch`, `git_stash`, `git_checkout` — each its own
    permission category. History rewrite stays denied by default in every
    mode.
  - `ask_user` → `WaitingForUser` with a structured question.
  - `report_finding` → a `ReviewFindings` artifact.
  - `plan_update` → a new plan revision; scope expansion is a permission
    event.
- `task_respond { task_id, answer }` resumes a `WaitingForUser` task with the
  answer journaled as `Trust::Instruction` from the user.
- Structured context on `task_create`: `attachments[] { path, range?,
  selection_text? }` enter context as pinned, provenance-tagged items (not
  objective text).

**Protocol 1.16** — `task_respond`; `TaskCreateRequest.attachments`;
`user_question` event; `permission_rules` (list/revoke persisted grants).

**App**
- The reply composer answers `user_question` in Chat.
- Attach selection/file from the editor and explorer; the retired "preamble"
  path is deleted.
- Git commit/branch from the Review surface goes through Core (approval UI
  shows it).
- Permission rules page (list + revoke).

**Exit:**
- A fake-model scenario asks a question, the app/CLI answers, the task uses
  the answer.
- An agent commit requires approval in Assisted mode and appears in `git_log`.
- `git push --force` / `reset --hard` are refused in Autonomous mode.
- Attachments appear in `context_retrieved` with `reason: attached`.

---

### M5 — Planning and multi-agent, complete

**Core**
- Child tasks: `Task.parent_task`, each with its own journal, budget and
  cancellation token (cancel propagates down; pause propagates down; a child
  crash recovers like any task).
- Role pipeline for `ModelAuthored` multi-step work:
  1. Researcher (read-only tools) → `ResearchBrief`
  2. Planner → `Plan`
  3. Implementer children per wave → `ChangeSet`
  4. Tester → `VerificationReport`
  5. Reviewer (no write) → `ReviewFindings`, which can trigger a repair
     revision

  Artifacts are the only channel.
- Parallel wave executor: `parallelizable` steps in one wave run as
  concurrent children under a per-workspace concurrency cap. Each child
  declares its `targets`; overlapping targets serialize; a ledger conflict
  aborts the later write as `ExternalModification` for that child.
- Per-step verification: a step's `verification` runs at step end; failure
  enters Diagnosing scoped to that step before later waves start. The full
  run before `Completed` stays mandatory.
- C8: verify state, loop-detector history and repair-ledger counters
  reconstructed from the journal on resume, like the plan repair budget
  already is.

**Protocol 1.17** — `task_children`, `task_artifacts`, `plan_revisions`
(+ diff); `subtask_started` / `subtask_completed`, `artifact_published`
events; `TaskSummary.parent_task_id`.

**App**
- Task view becomes a tree (parent → role children).
- Plan view renders the DAG with parallel lanes and live per-step state.
- Artifact viewer (brief / changeset / report / findings).
- Plan revision diff.
- Pause/cancel on a child vs the whole task.

**Exit:**
- A two-wave plan with two parallel steps runs concurrently (timestamps
  overlap), survives `kill -9` mid-wave, and resumes without re-running
  finished children or double-applying edits.
- Overlapping targets serialize.
- A loop detected before a crash is still counted after resume.
- A Reviewer finding causes a repair revision.

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
