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

Last updated: 2026-08-29 (after Phase 10).

---

## Phases

| Phase | Scope | Status |
|---|---|---|
| 0 | Foundations: workspace, toolchain pin, CI, `types`, `util`, `store`, `events`, `config`, `testkit`, `xtask` | **done** |
| 1 | Platform: `vfs`, `process`, `sandbox`, `hardware`, `git` | **partial** — macOS and permissive sandboxes only; no Linux or Windows confinement |
| 2 | Execution: `permissions`, `tools`, `edit`, `ledger` | **partial** — 16 of 18 tools live; all 6 edit strategies live as of Phase 4 |
| 3 ⭐ | Walking skeleton: `model` + `runtime-fake` + minimal `orchestrator`, `task`, `agent`, minimal `context`, `protocol`, `app`, `cli` | **done** |
| 4 | Repository intelligence: `lang`, `index`, `graph`, incremental pipeline, `lsp` | **partial** — 7 languages of the 11 planned tier-1 set; no large-repo performance run yet |
| 5 | Search: `embed`, `search`, fusion ranking, explanations | **partial** — engine and all seven modes live; not yet wired to the `search` tool or the agent loop, and semantic ranking rides a placeholder embedder until Phase 9 |
| 6 | Context, instructions, memory; prompt assembly with the trust lattice | **partial** — the full pipeline, instruction discovery and memory are implemented and tested against every stated exit criterion; a search-backed `Retriever` is included, but wiring retrieval + index bootstrap into the live agent loop is a deliberate follow-up (the embedded runtime still drives with explicit-file context) |
| 7 | Verification, diagnosis, repair: `verify`, failure parsers, repair loop, loop detection | **partial** — discovery, escalation strategy, the runner, ten failure parsers, diagnosis, the completion report, five loop detectors and the driver's verify→diagnose→repair loop are all implemented and tested; a fake-model seeded-bug fix and a caught non-converging loop pass end to end. The 30-real-repo discovery corpus and the captured-output parser corpus at scale wait on `valyria-bench` (Phase 11), and mapping changed symbols → covering tests / graph-neighbour suspects in the *live* loop is a Phase 6/8 wiring follow-up |
| 8 | Planning and multi-agent: `plan`, checkpoints, rollback boundaries, sub-tasks | **partial** — the plan model, validator (nine structured error codes), dependency/wave scheduler, checkpoint capture + ledger-backed rollback, the five multi-agent roles + typed artifacts, and the `PlanStore` (migration block 800-899) are implemented and tested; the driver runs `Planning` as a model-authored, validated, bounded-repair plan and executes it step by step with a checkpoint at each rollback boundary. Deferred: spawning a role as its own child task (sub-agents), real parallel step execution, index-backed target resolution, and verification interleaved *between* steps (the mandatory full `Verifying` run is the backstop) |
| 9 | Real models: `runtime-llamacpp`, `runtime-mlx`, `runtime-openai-compat`, registry, store, model pool | **partial** — the embedded `ModelCard` catalog + hardware-fit selection, the verified resumable download store (migration block 900-999), the `OpenAiCompatRuntime` adapter (behind an `HttpTransport` seam), and the orchestrator's tool-call transport ladder + memory-aware model pool + fallback-chain router are all implemented and tested offline. Deferred (open decision 5, solo build): the concrete `reqwest` transport, the real GGUF-loading probe, the `runtime-llamacpp` / `runtime-mlx` adapters, and wiring the ladder/pool/router into the live agent loop |
| 10 | Interface completion: protocol v1 freeze, schema export + compat gate, full CLI, TUI, `doctor`, `clean`, `global.db`, daemon | **partial** — the frozen v1 protocol (all workflow operations), the schema-export + `xtask check-protocol` CI gate, the `SocketClient`/`serve` daemon transport, `GlobalStore` (`~/.valyria/global.db`), `Doctor` (ten checks), `StorageInspector` (`inspect`/`purge`), the full CLI subcommand tree with `--json`/`--connect`, and the ratatui TUI session are all implemented and tested. Deferred: a multi-workspace daemon, length-prefixed framing, TypeScript type export, and `doctor`'s live sandbox self-test |
| 11 | Hardening and evaluation: `bench`, fuzzing, perf work, cross-platform matrix, release gates | not started |

Phase 9 can start early against the OpenAI-compatible adapter (a locally running
`llama-server`) without waiting for any FFI work.

---

## What Phase 10 delivered

The runtime is now **drivable end to end through one frozen protocol** — in
process, over a Unix-socket daemon, from a full CLI, or from a TUI — and it
can inspect and repair its own environment.

**The protocol is a frozen v1 (`PROTOCOL_VERSION = 1.0.0`).** `Request` /
`Response` cover every workflow: `task_create` / `status` / `list` /
`report` / `plan` / `rollback` / `pause` / `resume` / `cancel`,
`permission_resolve`, `events_subscribe`, plus `workspace_status`,
`doctor_run`, `storage_inspect` / `storage_purge`, `config_show`,
`memory_list` and `model_list`. Every wire type derives
`schemars::JsonSchema`; `valyria_protocol::schema::export` renders
`docs/protocol/{request,response,event}.schema.json` + `version.txt`, and
`cargo xtask check-protocol` (a new CI job) fails any drift that did not
also bump `PROTOCOL_VERSION` — the §4.27 machine-checked compat gate,
alive.

**The daemon is a pure backend swap.** `valyria_protocol::transport`
adds newline-delimited JSON framing and `SocketClient`, the
daemon-transport implementation of the *same* `Client` trait the embedded
runtime implements. `valyria_app::daemon::serve` binds a Unix socket and
dispatches each framed frame straight into an `EmbeddedClient`, so the
socket path and the in-process path run identical runtime code (the point
of D11). `valyria serve` runs it; `valyria --connect <socket> <anything>`
uses it — no other CLI code path changes. The frame enums are externally
tagged *on purpose*: `WireEvent.payload` is a `serde_json::Value`, which
does not survive serde's tagged-enum content buffer.

**`global.db` finally has an opener.** `GlobalStore` opens
`~/.valyria/global.db` (`$VALYRIA_HOME` override) — the §4.1 assembly
point concatenating the installed-model index (block 900-999), user-scoped
memory (600-699) and a new `workspace_registry` (block 10_100+). Every
`Runtime::open` registers its workspace there.

**`doctor` runs a real battery.** Ten checks — runtime build, data-dir
writability, `workspace.db` `integrity_check`, disk headroom, git health,
sandbox confinement, inotify watch ceiling, permission-config vs the
policy floor, index presence, installed models — each returning
`pass` / `warn` / `fail`, a plain-language detail, and a concrete
remediation. Each check is a free function taking only its inputs, so the
"diagnoses deliberately broken environments" criterion is tested by
handing each one a broken input directly (a corrupt db file, a missing
directory, a non-git workspace).

**`clean` is built from inspection.** `StorageInspector::inspect` sizes
every on-disk area (`workspace.db`, blobs, index, cache, tasks,
`global.db`, models, logs); `purge(scope, dry_run)` reclaims `memory` /
`cache` / `tasks` / `logs`, and `--dry-run` reports what it *would* free
without touching anything.

**The CLI is complete and the TUI is real.** Full subcommand tree with a
global `--json` and `--connect`; `valyria` with no arguments opens a
ratatui session — task list, live event log, compose a new objective,
pause/resume/cancel and allow/deny on the selected task — all through the
`Client` trait, so it works identically against a daemon.
`crates/valyria-cli/tests/phase10.rs` drives the real binary for `doctor`,
`status`, `config`, `model list`, `clean --dry-run`, and a full `serve` +
`--connect` round trip.

~70 new tests; 1058 pass across the workspace, up from 1020.

---

## What Phase 9 delivered

The model layer stopped being a single scripted fake. There is now a
**catalog**, a **verified weights store**, a **real runtime adapter**, and
the **transport ladder** that turns an unreliable open-weight model into a
usable one — all runnable and tested with the network disabled.

**The catalog is embedded and fit is measured.** `valyria-model-registry`
ships `catalog.json` compiled into the binary: one `ModelCard` per
quantization variant, carrying its `ModelRequirement`, per-role suitability
scores, transport preference, license, source URL and blake3 hash.
`select_for_role` runs candidates through `valyria_hardware::fits` — the
*same* function `doctor` uses — and penalises a `Tight` fit so a
comfortably-fitting, slightly-less-suitable model can legitimately win.
`RoleBinding::derive` turns "which model for the planner?" into a primary
plus an ordered fallback chain.

**Downloads are resumable, verified, and never partial-on-success.**
`valyria-model-store`'s `plan_install` surfaces size + license + hardware
fit and returns a plan the caller must `.confirm()`. `install` streams the
weights through the `Fetcher` seam into a `.part` file, resumes from its
length on a retry, then runs a **whole-file blake3 check** — a mismatch
deletes the file and hard-errors rather than leaving a broken install.
A `Prober` seam runs the post-install load/generate probe; `manifest.json`
is written atomically. `verify_integrity`, `remove` (reports freed bytes),
`gc` (drops models outside the keep-set, sweeps stray partials) and
`storage_report` are the reclamation surface; migration block 900-999
holds the rebuildable `installed_model` index.

**One adapter covers every local server.** `OpenAiCompatRuntime`
implements `ModelRuntime` against llama-server / vLLM / Ollama / LM Studio.
HTTP is behind the `HttpTransport` trait, so `/v1/chat/completions`
request building, buffered *and* SSE parsing, native tool-call extraction,
`/health`, and cancellation on both the buffered and streaming paths are
all asserted against a scripted `MockTransport`. Mid-stream cancel stops
the stream and emits a terminal `Cancelled`.

**The transport ladder is the headline (D5).**
`valyria_orchestrator::structured` reads a tool call out of a completion
in three tiers: the adapter's native `tool_calls` array; failing that, a
tolerant recovery parser over the model's text — it strips ```` ```json ````
fences, `<tool_call>` / `<|python_tag|>` / `[TOOL_CALLS]` wrappers and
prose, pulls the first *string-aware* balanced JSON value (a `}` inside a
string literal does not unbalance it), tolerates trailing commas, and
accepts every common shape (`{name,arguments}`, `{tool,args}`,
`{function:{…}}`, `{tool_call:{…}}`, arrays, stringified arguments);
failing *that*, `resolve_tool_calls` feeds the parse error back to the
model as evidence and asks again, bounded by a retry budget. A 21-case
corpus of real open-weight output shapes is the regression guard.

**The pool protects the critical path.** `ModelPool` admission control is
LRU-within-role-priority: a background embedder or reranker is evicted
under memory pressure, the primary coder never is — an embedder that can
only fit by displacing the coder is refused (`WontFit`) instead.
`ResourcePressure`, `Evicted` and `Loaded` events explain every decision.
`RoleRouter` walks a binding's fallback chain, skipping unregistered or
unhealthy models and retrying the next on a retryable error.

**Deliberate scope choices for this phase (open decision 5 — a solo build
defers the MLX/CUDA adapters):** the concrete `reqwest`/`hyper`
`HttpTransport` impl, the real GGUF-loading `Prober`, and the
`valyria-runtime-llamacpp` / `valyria-runtime-mlx` adapters remain
documented scaffolds — the first is a ~60-line trait impl, and llama.cpp's
managed-server mode reuses `OpenAiCompatRuntime` wholesale. The ladder,
pool and router are built and tested but the live agent loop still drives
through the Phase 3 `Orchestrator`; wiring them in (with catalog-backed
role bindings and a real `ModelRuntime` behind `PrimaryCoder`) is the
Phase 9 follow-up.

78 new tests; 1020 pass across the workspace, up from 942.

---

## What Phase 8 delivered

The runtime can now **turn a task into a validated plan and execute it step
by step**, checkpointing at rollback boundaries and refusing to clobber a
developer's work on the way back.

**The model proposes; the runtime validates.** A plan is a DAG of
`PlanStep { id, intent, targets, depends_on, parallelizable, checkpoint,
verification, rollback_boundary, approval_required, estimated_scope }`.
`valyria_plan::validate` runs nine checks — unique ids, resolvable
dependencies, acyclicity (with the cycle path in the message), a
verification on every mutating step, a checkpoint on every rollback
boundary, targets inside `plan_scope`, `plan_scope` inside the permission
profile, no unresolvable targets, no empty plan — and returns **every**
failure at once, each a machine `PlanErrorCode`, not a prose paragraph.

**Invalid plans are repaired, not rejected.** `Planning` (in
`PlanningMode::ModelAuthored`) asks the model for a `submit_plan`, and a
`PlanRepairLedger` hands the structured errors back for up to three bounded
rounds before failing the task. The repair budget is durable: it is
rebuilt from the journal's `plan_rejected` count on resume, and the raw
plan submission is stored *inside* the planning `model_completion` entry so
a crash between "model answered" and "runtime decided" re-processes that
submission rather than re-calling the model (which would desync the shared
turn counter).

**Plans are living, revisable, diffable documents.** Every accepted
revision is stored in `plan_revision` (migration block 800-899) with its
parent's content hash; `Plan::diff` reports added / removed / changed steps
between revisions.

**Execution walks a schedule, durably.** `valyria_plan::schedule` groups
steps into dependency waves (parallelizable steps bucketed within a wave,
for a later concurrent executor); the driver runs one step at a time.
"Which steps are done / started / checkpointed" is rebuilt from the task
journal, never process memory, so a `kill -9` mid-plan plus `valyria task
resume` picks up at the next incomplete step without re-running the
finished ones or double-applying an edit.

**Checkpoints are markers; rollback is the ledger's job.** A
`checkpoint`-flagged step captures the task-touched file set with their
on-disk hashes and a change-ledger watermark. `AgentDriver::
rollback_to_checkpoint` replays every ledger entry after the watermark in
reverse through `Ledger::rollback_entry`, which already refuses
(`RollbackConflict`) to revert a file anyone — the user included — has
touched since; the first such refusal aborts the whole rollback with the
offending path and leaves the tree exactly as it was. A clean rollback is
verified: every checkpointed file must hash back to what the checkpoint
recorded.

**Multi-agent is roles + typed artifacts.** `AgentRole`
(Researcher / Planner / Implementer / Tester / Reviewer) carries a tool
allowlist, a `can_write` flag (only the Implementer), and a permission
ceiling; the five `Artifact` types (`ResearchBrief`, `Plan`, `ChangeSet`,
`VerificationReport`, `ReviewFindings`) are the only inter-role channel and
persist in `task_artifact`. Spawning a role as its own child task is a
documented follow-up.

**Deliberate scope choices for this phase:** no child-task sub-agents, no
real parallel step execution, target resolution against the workspace
filesystem rather than the repository index (the index is still not wired
into the agent loop), and no verification interleaved between plan steps —
the mandatory full `Verifying` suite after the last step (Phase 7's
machinery) is the backstop, and per-step `verification` is still enforced
*structurally* by the validator.

46 new tests; 942 pass across the workspace, up from 896.

---

## What Phase 7 delivered

The runtime can now **run the repository's own checks, understand a
failure, and try to fix it** — and know when it is going in circles.

**Discovery reads the repo, then proves each command.** `valyria-verify`
scans `Cargo.toml`, `package.json` `scripts`, `pyproject.toml`, `go.mod`,
`Makefile` / `justfile` targets, tool config files and — ranked highest,
because they are the commands the maintainers actually run —
`.github/workflows/*` `run:` steps. Discovery itself spawns nothing; a
separate `validate` step runs one cheap probe per program and only
commands that actually launch on this machine are trusted.

**The strategy is cost/value, not a fixed script.** Steps are ordered
`Syntax → TargetedTest → RelatedTests → Style → Full` by
regression-catch probability per second, with two hard rules: a failing
check goes straight to diagnosis (no point running the slower ones), and
the full suite must run before `COMPLETED` — a green targeted test is not
evidence the whole thing still builds.

**A verification run is the only way to get verification `Evidence`.**
The runner executes one command through `valyria-process` under the
workspace sandbox, classifies `Passed / Failed / Errored / TimedOut`,
parses the output, and mints the `VerificationRunId` that `Evidence`
(D4) requires. Runs persist to `workspace.db` (block 700-799) and the
completion report is assembled from those rows alone — a model's "tests
pass" with no passing run in the log is reported as *not verified*.

**Ten failure parsers, one distilled shape.** cargo (both
`--message-format=json` and human), rust libtest (old and new panic
formats, `assertion left == right`), pytest, `go test` / `go build`,
jest / vitest, tsc, mypy, eslint, formatters (rustfmt / gofmt /
prettier), and a tolerant generic fallback that still records a
`file:line` and an error line when nothing specific matches. Each
produces `Failure { kind, primary_location, assertion, failing_test }`
with a line/column-independent `fingerprint` for loop detection.

**Diagnosis is an intersection.** A file that a failure points at *and*
that this task changed outranks either signal alone; a changed file in
the graph neighbourhood of a failure is next; the failing test's own
file is a weak suspect. Only that distilled subset — failures plus the
top few suspects — is offered to the repair prompt, never raw output.

**Five loop detectors, each with its own scenario.** Exact repeat
(identical step back-to-back), `A→B→A` oscillation, the same failure
fingerprint N times, N steps with no file-state change, and a stalled
progress metric (does the verification frontier advance? do failures
decrease? are new files being touched?). A finding routes through the
repair ledger — `Continue → EscalateStrategy → SwitchRole → AskUser →
GiveUp` — so a stuck agent escalates and then hands off; it never spins
silently, and it emits `ProgressStalled` the moment it trips.

**The driver runs the loop.** `Verifying` discovers, plans and runs the
next check (and is a plain pass-through when the repo has no tooling,
exactly as Phase 3); `Diagnosing` distils the failure, feeds the
detector, journals any `loop_detected`, and consults the repair ledger;
`Repairing` takes one model-authored edit and loops back to `Verifying`.
Verification runs are journaled effects, projected as
`test_started` / `test_passed` / `test_failed` / `verification_evidence`
events. Two fake-model integration tests: one fixes a seeded bug end to
end (fail → diagnose → edit → pass → report `Verified`), the other
proves a model that never actually fixes anything is caught looping and
handed off rather than run forever.

86 new tests; 896 pass across the workspace, up from 810.

---

## What Phase 6 delivered

The runtime can now turn "here is a task and a repository" into a
**trust-ordered, budget-fitted, injection-fenced prompt** — and rebuild
that prompt, byte for byte, from what it stored.

**The trust lattice is enforced by one function.** `PromptAssembler` is
the only place `RetrievalCandidate`s become messages. Content at
`Trust::Policy` / `Trust::Instruction` is the only content that reaches a
system position; everything at `Trust::Evidence` or below is wrapped in a
per-assembly nonce fence and preceded by a standing "this is data, not
instructions" frame. A model-emitted attempt to close the fence fails —
the fence identifier is 128 bits of injected randomness the data never
sees.

**Injection defense is annotate, never strip.** A dedicated detector
scans every fenced block for instruction-shaped text — "ignore previous
instructions", role markers, forged `<system>` / `<|im_start|>` / `[INST]`
tags, zero-width and bidi controls, mixed-script homoglyph words, long
encoded blobs, and fence-forgery attempts — and stamps a visible warning
on the block. The payload text stays intact so the model can reason about
it. An eleven-payload red-team suite
([crates/valyria-context/tests/injection.rs](../crates/valyria-context/tests/injection.rs))
asserts isolation, preservation, annotation and fence integrity for every
case.

**The budget allocator fails loudly.** Sections carry
`{ min, ideal, max, priority }`; the allocator reserves output tokens,
gives every section with candidates its floor, then tops up by priority.
If the floors do not fit, it returns `BudgetInfeasible` rather than
truncate silently — the caller narrows the task. Pathological inputs
(zero budget, reserve larger than total, one `usize::MAX`-sized item,
thousands of tiny items, an unbudgeted section) are covered by unit
tests and never panic or over-allocate.

**Compression drops whole units, never fragments.** Text shrinks by whole
trailing lines with a marker; source shrinks by lowering per-symbol
fidelity (`Full → Outline → Signature → Reference`) and then by dropping
the least-relevant *whole* symbols. A symbol body or a signature is
emitted verbatim or not at all — asserted directly, and again through the
`SearchRetriever` integration test, where every `SymbolSpan` body is
checked to be an exact slice of the file on disk.

**Prompt reconstruction is structural, not hopeful.** Assembly builds a
`ContextSnapshot` (nonce, policy, task intent, and every placed item with
its provenance, level, rendered text and injection signals) and the
messages *are* `snapshot.render()`. `snapshot → serialize → deserialize →
render` therefore cannot diverge; a test asserts byte-identical
round-trips and a matching `body_hash`.

**Instruction discovery has a fixed, documented authority order.**
`valyria-instructions` walks the workspace for `~/.valyria/instructions.md`,
`VALYRIA.md`, `AGENTS.md`, `CLAUDE.md`, directory-scoped files
(nearest-to-the-edited-file wins) and the advisory `CONTRIBUTING.md` /
`README`. Each source gets a trust level — everything actionable is
`Trust::Instruction`; advisory files are `Trust::RepoData`, mined for
facts but never obeyed. Oversized files truncate at a line boundary; a
whole-set fingerprint makes "re-read on change" a cheap comparison; a
conservative heuristic reports two directives that contradict each other,
with the higher-authority one always the winner.

**Memory has four tiers, decay, and a delete surface.**
`valyria-memory` stores session / task / repository / user entries in a
new `workspace.db` block (600-699). Agent-extracted entries are
`Trust::Evidence` (they inform, not command); user-authored entries are
`Trust::Instruction` and do not decay. Everything else halves in
confidence every 30 days of silence and is revived by being retrieved;
entries contradicted by evidence are retired. Retrieval scores term
overlap × decayed confidence, pins session memory to the header, and
`purge` backs the eventual `valyria clean --memory`.

**Retrieval is a seam.** `ContextEngine` runs the whole pipeline over
whatever a `Retriever` provides. `StaticRetriever` is the default;
`SearchRetriever` (behind the `intelligence` feature, on by default) runs
`valyria-search` and turns its ranked, explained hits into source
candidates carrying the hit's own `Provenance` — so `context.explain`
gets "why this file" straight from what search recorded. Wiring
`SearchRetriever` and an index bootstrap into every `valyria run` is left
as a focused follow-up.

90 new tests; 810 pass across the workspace, up from 720.

---

## What Phase 5 delivered

The runtime can now be asked "which files matter for this?" and give a ranked,
explained answer.

**Semantic retrieval works without a model.** `valyria-embed`'s `Embedder`
trait is what `valyria-model` will implement once a real embedding model is
loaded (Phase 9). Until then, `HashingEmbedder` produces deterministic
feature-hashed vectors offline — a modest but real retrieval signal, and one
that lets the search tests assert exact rankings. Semantic search is treated
everywhere as *one ranked input among several*, never the sole authority.

**Vectors are generational, like the index.** Every row is stamped with the
index generation it was derived from, so a search at generation *N* sees the
vectors for *N*. A rebuild for a new generation copies forward the vector of any
chunk whose content hash is unchanged and only re-embeds the rest — the
chunk-level invalidation §4.15 calls for.

**The nearest-neighbour index is checked, not trusted.** `EmbedStore::search`
(HNSW) and `EmbedStore::search_exact` (brute-force cosine) sit side by side and
a test asserts they agree on the top results, because an approximate index that
is subtly wrong has no symptom of its own — the same reasoning behind
`verify_index`.

**Seven modes, one ranked list.** Lexical (TF-IDF-weighted content scan folded
with the symbol FTS), regex, symbol, semantic, AST (tree-sitter query
patterns), dependency (graph traversal from the task's anchor files) and git
(recent history) each produce a ranked list of files; reciprocal-rank fusion
combines them and a feature reranker adjusts for recency, git churn,
import-graph distance from the anchors, test proximity and a path prior. A mode
with nothing to contribute — no embeddings, not a git repo, no anchors — returns
a `degraded` note rather than an error, and **search works fully with
embeddings disabled** (a Phase 5 exit criterion, asserted).

**Every result explains itself.** Each hit carries a `ScoreExplanation` with the
per-mode stage scores, every reranking feature's weighted contribution, and the
ordered retrieval path — and `hit.score` is set from exactly that feature sum,
so a test can (and does) assert the number never drifts from its own
explanation. This is the same data `SearchHit::provenance()` hands to
`context.explain` (§14).

**Ranking has a regression guard.** A labeled retrieval set — "which files must
be touched to answer this?" over a fixture repository — is scored by recall@5
and mean reciprocal rank in CI, with margin left for a real embedder to
*improve* the numbers.

56 new tests; 720 pass across the workspace, up from 664.

---

## What Phase 4 actually delivered

The runtime now understands the code it is editing, not just its bytes.

**Language support is data, not code** (D9). A language is a directory of `.scm`
queries plus a ~40-line provider; one extraction engine, driven entirely by
capture names, serves all of them. Rust, Python, Go, Java, JavaScript,
TypeScript and TSX ship today, each behind its own cargo feature. Extraction
produces qualified symbol paths (`Parser::parse`, `Outer.Inner.method`),
imports, call sites, tests, doc comments and signatures — proven by a per-
language corpus in
[crates/valyria-lang/tests/extraction.rs](../crates/valyria-lang/tests/extraction.rs).

**The index is generational** (D8). Every row records the generation range it
was valid for, so a read at generation *N* sees the repository exactly as it was
when *N* was published, however far the index has moved on. A long agent step
therefore never has the index shift underneath it, and "was this planned against
stale context?" is a comparison of two integers. Reading at a pruned generation
fails loudly rather than quietly answering from newer data.

**Index drift is tested for, not hoped about.** `verify_index` rebuilds from
scratch, independently of the incremental pipeline, and diffs the result against
what the index believes. A ten-round fuzz of edits, creations, renames and
deletes ends with zero drift — and the check is shown to be capable of failing,
by editing files behind the index's back.

**The graph is honest about what it does not know.** Without a type checker,
calls resolve by name, so every edge records *how* it was derived: `Exact` for
structure, `Likely` for a unique match after narrowing by file and imports,
`Ambiguous` when several candidates remain — recorded to all of them rather than
resolved by coin flip. References that leave the repository (`serde`, `println`)
are kept as unresolved references, because "this file depends on serde" is a
real fact about the code.

**LSP is enrichment, never a dependency.** Every entry point returns an empty
answer rather than an error when a server is missing, slow, crashed, or rejects
a request — so a machine with nothing installed works, it just gets index-derived
results. The client is generic over its streams, so lifecycle, request
correlation, timeouts, server-initiated requests, crashes and malformed frames
are all tested against a scripted in-process server on every machine.

**The editing ladder is complete.** Symbol-aware replacement resolves against
the file's *current* content rather than the index — the index says which file
to edit, but only the bytes on disk can position the edit safely. AST transforms
are a closed set of typed operations (rename, delete, insert, query-driven
replacement), not a free-text description, so they can be executed, verified and
replayed from a journal. And §4.11's re-parse guard now applies to *every*
strategy: an edit that introduces syntax errors into a file that parsed cleanly
is refused and nothing is written.

664 tests pass across the workspace, up from 431.

---

## What Phase 3 delivered

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

---

## Known gaps

Things that exist as an interface but not yet as behaviour. Each returns a
clear "not implemented in this phase" error rather than pretending:

| Gap | Lands in |
|---|---|
| `search` and `symbol_search` tools return `tools.not_yet_implemented` — `valyria-search` implements the engine and `valyria-context`'s `SearchRetriever` consumes it, but the first-class tools and CLI still do not | Phase 6 follow-up |
| The index, graph and search engine are not wired into the *live* agent loop: nothing calls `bootstrap` during a task, and the embedded runtime drives with explicit-file context. `ContextEngine` + `SearchRetriever` are built and tested; wiring them into `valyria-app`/`valyria-agent` (with a bootstrap and generation-pinning strategy) is the follow-up | Phase 6 follow-up |
| Sandbox on Linux and Windows falls back to `PermissiveSandbox` (reported, never silent) | Phase 1 completion |
| No concrete HTTP transport: `OpenAiCompatRuntime` is generic over `HttpTransport`, tested against `MockTransport`; the `reqwest`/`hyper` impl that talks to a real `llama-server` is not written. `valyria-runtime-llamacpp` (managed-server mode) and `valyria-runtime-mlx` remain scaffolds | Phase 9 follow-up |
| The transport ladder, `ModelPool` and `RoleRouter` exist and are tested, but the live agent loop still drives through the Phase 3 `Orchestrator` with a single bound runtime; catalog-backed role bindings + a real `ModelRuntime` behind `PrimaryCoder` are not wired into `valyria-app` | Phase 9 follow-up |
| The post-install probe uses `NullProber` (records the card's declared capabilities); the real load-and-generate probe needs a GGUF-loading adapter | Phase 9 follow-up |
| `global.db` now has an opener (`valyria_app::GlobalStore`: installed-model index + user memory + workspace registry). Still standalone: user-scoped config lives in `config.toml`, not the DB, and there is no cross-machine sync | — |
| Model-authored planning is opt-in (`--plan` / `PlanningMode::ModelAuthored`); the default `Planning` is still the Phase-3 pass-through so every pre-Phase-8 scenario is unchanged. Turning it on by default waits on the live-loop model wiring | Phase 9 follow-up |
| Multi-agent roles + typed artifacts exist, but a role is not yet run as its own child task (own journal, budget, cancellation); the driver executes one flat plan | Phase 8 follow-up |
| Plan steps run one at a time even when `parallelizable`; the scheduler computes the parallel groups but the executor does not use them yet | Phase 8 follow-up |
| Plan `targets` are validated against the workspace filesystem, not the repository index (still not wired into the agent loop); no verification is run *between* plan steps — the mandatory full `Verifying` suite is the backstop | Phase 6/8 follow-up |
| The verify→diagnose→repair loop detector + repair ledger are process-local per task run (the plan, its revisions, checkpoints and verification runs are all durable) | Phase 8/11 |
| The `--connect` daemon owns a single workspace (one `Runtime` behind the socket); a multi-workspace daemon with `workspace.open/close` routing is a follow-up | Phase 10 follow-up |
| `xtask schema` exports JSON Schema only; the TypeScript type export §4.27 also mentions, `agent.events` length-prefixed framing, and `doctor`'s live sandbox self-test are follow-ups | Phase 10 follow-up |
| `valyria memory list` needs a query — a browse-all memory surface over the protocol is a follow-up | Phase 10 follow-up |

### Phase 4 exit criteria not yet met

The crates are built and tested; two of the phase's stated exit criteria are
performance claims that need a measurement harness (`valyria-bench`, Phase 11)
before they can honestly be asserted:

- **Index a 100k-file repo within target; incremental update p95 < 200ms.** The
  design is built for it — parallel scan, one write transaction, a staged
  files-first generation — but no benchmark has been run, so the numbers in
  [PLAN.md §9](PLAN.md#9-platform-and-performance-targets) are still targets
  rather than results.
- **`verify-index` shows zero drift after a 10k-operation fuzz.** A deterministic
  ten-round fuzz of edits, creations, renames and deletes passes in CI today;
  scaling it to 10k operations belongs with the bench harness.

Two further gaps are deliberate scope choices rather than unfinished work:

- **Tier-1 languages: 7 of the 11 planned.** Rust, Python, Go, Java, JavaScript,
  TypeScript and TSX — decision 6's "cut to five ecosystems" list. C/C++, C#,
  Ruby, PHP, Kotlin and Swift, and the tier-2 structure-only set, are each a
  `queries/<lang>/` directory plus a small provider, with no change to
  extraction, indexing, the graph or search.
- **LSP servers are configured but unexercised against real ones.** The client
  is fully tested against a scripted server; behaviour against a real
  rust-analyzer or gopls is untested, and belongs in the Phase 11 matrix.

### Phase 10 exit criteria: status

- **A client can drive every workflow through the protocol alone.** Done —
  `Request` / `Response` cover create / status / list / report / plan /
  rollback / pause / resume / cancel / permission plus workspace status,
  doctor, storage inspect + purge, config and model list;
  `crates/valyria-cli/tests/phase10.rs` drives the real binary through
  them, and `daemon_serves_the_same_protocol_over_a_unix_socket` proves
  the same set works unchanged over `--connect`. `EmbeddedClient` and
  `SocketClient` are the two implementations of one `Client` trait.
- **Schema-compat CI gate active.** Done — `valyria_protocol::schema::
  export` renders `docs/protocol/`, `cargo xtask check-protocol` diffs the
  committed files against the live types and fails on drift, the `protocol`
  job in `.github/workflows/ci.yml` runs it, and
  `xtask::tests::committed_protocol_schema_is_current` runs it in the unit
  suite too. `PROTOCOL_VERSION` is `1.0.0` with a documented bump policy.
- **`doctor` correctly diagnoses a battery of deliberately broken
  environments.** Done — `doctor::tests` feed each check a broken input
  (`corrupt_workspace_db_is_detected`, `missing_data_dir_fails`,
  `non_git_workspace_warns_not_fails`, the disk / watch-limit threshold
  tables) and `phase10.rs::doctor_flags_a_non_git_workspace` asserts it
  through the real binary. Each check returns a status, a detail, and a
  remediation.
- **Deliberate scope choices:** the daemon is single-workspace, framing is
  newline-delimited (not length-prefixed), the schema export is JSON
  Schema only (no TypeScript), and `doctor`'s sandbox check reports the
  platform's known confinement rather than running a live self-test. None
  of the criteria above depend on that further work.

### Phase 9 exit criteria: status

- **A real local model completes the Phase 7 seeded-bug suite.** Not yet —
  this needs the concrete `HttpTransport` impl and a running `llama-server`,
  which are the documented follow-up. The pieces it depends on are proven:
  `OpenAiCompatRuntime` drives a full `generate` (buffered and streamed),
  native-tool-call, health and cancellation cycle against `MockTransport`
  (`crates/valyria-runtime-openai-compat/tests/runtime.rs`), and the
  transport ladder recovers a tool call from a bad turn and retries
  (`crates/valyria-orchestrator/tests/transport_ladder.rs::reformat_retry_recovers_after_one_bad_turn`).
- **Model install verifies integrity and surfaces license.** Done —
  `crates/valyria-model-store/tests/install.rs`:
  `plan_surfaces_size_license_and_fit`,
  `happy_path_downloads_verifies_probes_and_writes_manifest`,
  `integrity_mismatch_deletes_the_download_and_leaves_nothing_installed`,
  `interrupted_download_resumes_from_the_part_file`. The whole-file blake3
  check is mandatory before a manifest is written.
- **Memory pressure triggers eviction rather than OOM.** Done —
  `crates/valyria-orchestrator/src/pool.rs::pressure_evicts_lowest_priority_first_and_keeps_the_coder`,
  `::a_low_priority_model_will_not_evict_a_higher_priority_one`,
  `::lru_breaks_ties_within_a_priority`, `::a_model_bigger_than_the_whole_budget_wont_fit`.
  Admission returns `WontFit` or an eviction plan; it never over-commits the
  budget.
- **Mid-generation cancel actually stops the runtime.** Done at the adapter
  boundary —
  `crates/valyria-runtime-openai-compat/tests/runtime.rs::stream_stops_when_the_token_is_cancelled_midway`
  and `::generate_honours_a_pre_cancelled_token`: the buffered path races
  the cancel token with `tokio::select!`, and the SSE path stops pulling and
  emits a terminal `Cancelled` the moment the token trips.
- **Deliberate scope choice (open decision 5):** the `reqwest` transport,
  the GGUF-loading probe, and the llama.cpp / MLX adapters are follow-ups.
  Every criterion above that does not require a live server is met; the
  first is blocked only on that ~60-line transport impl.

### Phase 8 exit criteria: status

- **Invalid plans from the model are rejected with structured feedback and
  repaired.** Done —
  `crates/valyria-agent/tests/plan_loop.rs::invalid_plan_is_rejected_with_structured_feedback_and_repaired`:
  a cyclic plan is rejected with a `plan_rejected` journal entry carrying
  the `cyclic_dependency` code, the model's next `submit_plan` is accepted,
  and the task completes. `validate`'s nine codes each have a unit test in
  `crates/valyria-plan/src/validate.rs`, and `PlanRepairLedger` bounds the
  loop at three rounds (`repair.rs` tests).
- **A multi-step plan executes with a mid-plan pause/resume across a
  process restart.** Done — `plan_loop.rs::a_multi_step_plan_executes_step_by_step_with_a_checkpoint`
  proves the in-process half (two steps, per-step `plan_step_started` /
  `plan_step_completed`, one checkpoint), and
  `crates/valyria-cli/tests/walking_skeleton.rs::multi_step_plan_survives_kill_nine_and_resumes_mid_plan`
  drives the real binary: a plan run is `SIGKILL`ed mid-flight, resumed in
  a fresh process, and completes with each step's file written exactly
  once.
- **Rollback to a checkpoint restores the tree exactly and refuses on
  user-touched files.** Done — `plan_loop.rs::rollback_to_a_checkpoint_restores_the_tree_exactly`
  and `::rollback_refuses_and_leaves_the_tree_alone_when_a_file_was_touched_since`,
  plus `crates/valyria-plan/src/checkpoint.rs`'s unit tests, which drive
  the real `Ledger`.
- **Deliberate scope choices:** child-task sub-agents, real parallel step
  execution, index-backed target resolution, and verification interleaved
  between steps are follow-ups (see the Known gaps table). The exit
  criteria above are properties of the plan model, the validator, the
  scheduler, the checkpoint/rollback pair and the driver, and do not
  depend on that further wiring.

### Phase 7 exit criteria: status

- **Discovery finds correct commands across languages.** Done at fixture
  scale — `crates/valyria-verify/src/discovery.rs` tests cover cargo (incl.
  workspaces), `package.json` scripts with the right package manager,
  `go.mod`, `Makefile` / `justfile` targets, tool-config files, `sh`
  script conventions and CI `run:` extraction (including that CI outranks a
  manifest guess and that non-tool `run:` lines are ignored). Running the
  ≥30-real-repo corpus belongs with `valyria-bench` (Phase 11).
- **Parsers tested against a captured-output corpus.** Done at unit scale —
  `parse.rs` tests exercise every parser against representative real output
  (JSON and human cargo, both libtest panic formats, pytest summary +
  traceback, `go test` detail lines and `go build` errors, jest
  expected/received, tsc, mypy, eslint stylish, rustfmt `--check`), plus
  the dispatcher's timeout / generic-fallback / non-zero-with-no-parse
  paths. A large captured corpus is a Phase 11 asset.
- **A seeded-bug fixture is fixed end to end by the fake model.** Done —
  `crates/valyria-agent/tests/repair_loop.rs::seeded_bug_is_verified_diagnosed_and_repaired_end_to_end`:
  a `verify.sh` fixture fails, is diagnosed, the model edits the file, the
  re-run passes, and `CompletionReport::from_runs` reports `Verified` — all
  from the durable `verification_run` rows.
- **Every loop-detection class is triggered by a purpose-built scenario.**
  Done — `loop_detect.rs` has one test per class (exact repeat,
  oscillation period 2 and 3, repeated failure, no-change iteration,
  stalled frontier) plus resets and negative cases, and
  `repair_loop.rs::an_unfixable_bug_trips_loop_detection_and_is_handed_off`
  drives the whole thing through the real driver: a model that never fixes
  anything is caught, escalated, and handed to the user with a bounded
  number of verification runs and a `progress_stalled` event.
- **Deliberate scope choice:** the driver's live loop passes an empty
  graph-neighbour set to `diagnose` (no index/graph in the agent loop
  yet — the Phase 6 follow-up), and the loop detector / repair ledger are
  process-local. The exit criteria above are properties of the pipeline
  and the driver and do not depend on that wiring.

### Phase 6 exit criteria: status

- **Injection red-team suite passes.** Done —
  `crates/valyria-context/tests/injection.rs` runs eleven hostile payloads
  (instruction overrides, forged role/system tags, bidi and zero-width
  characters, homoglyphs, encoded blobs, fence forgery) and asserts each is
  isolated from the system message, preserved verbatim in a fenced block,
  annotated with a warning, and unable to close the nonce fence.
- **Budget allocator handles pathological inputs.** Done — `budget::tests`
  covers zero budget, an output reservation larger than the total, a single
  `usize::MAX/2`-sized demand, an unbudgeted section, and a
  seven-section-all-maxed sweep; the allocator never panics and never hands
  out more than `available`, and returns `BudgetInfeasible` when floors do
  not fit.
- **Prompt reconstruction from stored provenance is byte-identical.** Done —
  the assembler builds a `ContextSnapshot` and the messages *are*
  `snapshot.render()`; a test asserts `serialize → deserialize → render`
  reproduces the messages exactly and the `body_hash` matches.
- **Context assembly stays under budget with no truncated-mid-symbol
  artifacts.** Done — an assembly test packs 60 source candidates into a
  3k-token budget and asserts `total_tokens ≤ available`; compression only
  ever emits a whole `signature`/`body` or drops a whole symbol, asserted in
  `compress::tests` and again in the `SearchRetriever` integration test
  (every `SymbolSpan` body is an exact slice of the file on disk).
- **Deliberate scope choice:** the `Retriever` seam is implemented and the
  search-backed impl is tested, but the embedded runtime is not yet wired to
  use it — see the Known gaps table. The four exit criteria above are all
  properties of the pipeline itself and do not depend on that wiring.

### Phase 5 exit criteria: status

- **Ranking evaluated against a labeled retrieval set.** Done, at fixture
  scale: `crates/valyria-search/tests/ranking_eval.rs` scores recall@5 and MRR
  over a labeled "which files must change?" set and fails below threshold.
  Building the set from real repositories at scale belongs with `valyria-bench`
  (Phase 11).
- **`--explain` output complete for every result.** Done. Every `SearchHit`
  carries a `ScoreExplanation`, `is_complete()` is asserted for every hit in
  the integration suite, and `hit.score` is set from the feature sum so it
  cannot disagree with its own breakdown.
- **Search works with embeddings disabled.** Done and asserted
  (`search_works_fully_with_embeddings_disabled`).
- **Deliberate scope choice:** semantic ranking rides `HashingEmbedder`, a
  deterministic offline stand-in, until a real embedding model lands with the
  model runtimes in Phase 9. The `Embedder` trait means that swap changes no
  code above it.

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
6. **Tier-1 language list** — 11 languages planned. **Phase 4 shipped the cut-to-
   five set** (Rust, TS/JS, Python, Go, Java) on the default, since D9 makes each
   further language additive: a `queries/<lang>/` directory and a small provider,
   with no change to extraction, indexing, the graph or search. Say the word if
   the full eleven should land before Phase 7's verification parsers.
