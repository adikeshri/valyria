# Changelog

All notable changes to this project are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
this project will follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
from 1.0.0. Before then, the protocol and every public API may change without
notice; see [docs/ROADMAP.md](docs/ROADMAP.md) for what is stable enough to
build on (nothing yet).

## [Unreleased]

Work toward the first release. Phases refer to
[docs/PLAN.md §5](docs/PLAN.md#5-build-phases).

### Added

- **Phase 8 — planning and multi-agent.** The runtime now turns a task into a
  validated, revisable plan and executes it step by step, checkpointing at
  rollback boundaries.
  - `valyria-plan` (new crate, migration block 800-899): the plan model — a DAG
    of `PlanStep { id, intent, targets, depends_on, parallelizable, checkpoint,
    verification, rollback_boundary, approval_required, estimated_scope }` — plus
    a **validator** that returns *every* problem at once, each a machine
    `PlanErrorCode` (empty plan, duplicate id, unknown dependency, cycle — with
    the path, mutating step without verification, rollback boundary without
    checkpoint, target outside `plan_scope`, `plan_scope` outside the permission
    profile, unresolvable target). A **scheduler** groups steps into dependency
    waves (parallelizable steps bucketed for a later concurrent executor).
    **Checkpoints** capture the task-touched file set + a change-ledger
    watermark; **rollback** replays the ledger entries after the watermark in
    reverse through `Ledger::rollback_entry`, which refuses on any file touched
    since — the first refusal aborts and leaves the tree untouched, and a clean
    rollback is hash-verified against the checkpoint. **Multi-agent** ships as
    `AgentRole` (Researcher / Planner / Implementer / Tester / Reviewer — tool
    allowlist, write ban, permission ceiling) plus the five typed `Artifact`s
    (`ResearchBrief`, `Plan`, `ChangeSet`, `VerificationReport`,
    `ReviewFindings`) persisted in `task_artifact`; `PlanStore` persists plan
    revisions (with parent hash), checkpoints and artifacts.
  - `valyria-agent`: `Planning` (opt-in `PlanningMode::ModelAuthored`, `--plan`
    on the CLI) asks the model for a `submit_plan`, validates it, and repairs
    invalid plans for up to three bounded rounds — the budget rebuilt from the
    journal on resume, the raw submission stored in the planning `model_completion`
    entry so a crash never forces a re-call. Plan-driven `Implementing` walks the
    schedule one step at a time; "which steps are done / started / checkpointed"
    is rebuilt from the journal, so `kill -9` mid-plan + `valyria task resume`
    continues at the next incomplete step with no re-run and no double-apply.
    `AgentDriver::rollback_to_checkpoint` (also on `Runtime`) exposes the
    checkpoint rollback. `plan_accepted` projects a `plan_created` event.
  - Deliberate scope: no child-task sub-agents, no real parallel step execution,
    target resolution against the workspace filesystem rather than the index,
    and no verification interleaved between steps — the mandatory full
    `Verifying` run is the backstop. A fake-model integration suite covers all
    three exit criteria; a CLI test `SIGKILL`s a plan run mid-flight and resumes
    it in a fresh process. 46 new tests; 942 pass across the workspace.

- **Phase 7 — verification, diagnosis, repair.** The runtime now runs the
  repository's own checks, distils a failure into a structured diagnosis, and
  drives a bounded repair loop that can be caught looping.
  - `valyria-verify`: tooling **discovery** scans manifests (`Cargo.toml`,
    `package.json` scripts, `pyproject.toml`, `go.mod`), `Makefile`/`justfile`
    targets, tool configs and — highest confidence — CI `run:` steps, then
    confirms each candidate by executing a cheap probe before it is trusted.
    An **escalation strategy** orders the confirmed commands by regression-catch
    value per second — syntax/type check first, a mandatory full run before
    `COMPLETED`, early exit on the first failure. A **runner** executes one
    command via `valyria-process` under the workspace sandbox, classifies the
    outcome and mints the `VerificationRunId` that makes the result
    verification-sourced `Evidence` (D4). **Failure parsers** for cargo (JSON +
    human), rust libtest, pytest, `go test`/`go build`, jest, tsc, mypy, eslint
    and formatters, with a tolerant generic fallback, produce a small
    `Failure { kind, location, assertion, failing_test }` set. **Diagnosis**
    intersects those locations with the change ledger (and, when wired, the
    graph neighbourhood) to rank suspect files. Runs persist to `workspace.db`
    (migration block 700-799); the **completion report** is built from those
    rows and nothing else — an unbacked "tests pass" claim is demoted to
    *unverified*.
  - `valyria-agent`: **loop and progress detection** — five detector classes
    (exact repeat, `A→B→A` oscillation, repeated failure fingerprint,
    no-change iteration, stalled verification frontier), each fed by a
    `StepSignature` / failure fingerprint / `ProgressMetric` and each covered
    by a purpose-built test. A **repair ledger** bounds the loop: `Continue →
    EscalateStrategy → SwitchRole → AskUser → GiveUp`, driven by attempt count,
    a regression, or a loop finding.
  - `valyria-agent::AgentDriver` wires it together: `Verifying` discovers,
    plans and runs the next check (a pass-through when the repo has no
    tooling, exactly as Phase 3); `Diagnosing` distils the failure, feeds the
    detector and journals a `loop_detected` / `progress_stalled` when it trips;
    `Repairing` takes one model-authored edit and loops back. Verification runs
    are journaled effects and projected as `test_started` / `test_passed` /
    `test_failed` / `verification_evidence` events. A fake-model integration
    test fixes a seeded bug end to end; another proves a non-converging loop is
    detected and handed off rather than spun on.

- **Phase 6 — context, instructions, memory.** A task and a repository become a
  trust-ordered, budget-fitted, injection-fenced prompt that can be rebuilt
  byte-for-byte from what was stored.
  - `valyria-context`: the full §4.17 pipeline. `PromptAssembler` is the one
    place candidates become messages and enforces the trust lattice (D3)
    structurally — only `Policy`/`Instruction` content takes a system position;
    everything at `Evidence` or below is wrapped in a per-assembly 128-bit
    nonce fence and framed as data. A dedicated detector annotates (never
    strips) instruction-shaped text — overrides, forged role/system tags, bidi
    and zero-width characters, homoglyphs, encoded blobs, fence forgery — with a
    visible warning; an eleven-payload red-team suite asserts isolation,
    preservation, annotation and fence integrity. The budget allocator carries
    `{ min, ideal, max, priority }` per section, reserves output tokens, and
    returns `BudgetInfeasible` rather than truncate silently. Compression drops
    whole lines or whole symbols (`Full → Outline → Signature → Reference`) and
    never a fragment of one. Assembly produces a `ContextSnapshot` whose
    `render()` *is* the message list, so `serialize → deserialize → render` is
    byte-identical. A `Retriever` seam with a `StaticRetriever` and a
    feature-gated `SearchRetriever` (turns `valyria-search` hits into
    provenance-carrying source candidates); `ContextEngine` runs the whole
    thing. The Phase 3 explicit-file `ContextAssembler` is unchanged and still
    what the embedded runtime drives with.
  - `valyria-instructions`: discovery with a fixed authority order —
    `~/.valyria/instructions.md`, `VALYRIA.md`, `AGENTS.md`, `CLAUDE.md`,
    directory-scoped files (nearest-to-the-edited-file wins), then advisory
    `CONTRIBUTING.md` / `README` (mined for facts, never obeyed). Trust
    assignment (`Instruction` vs. `RepoData`), a line-boundary size cap, a
    whole-set fingerprint for "re-read on change", and a conservative
    contradiction detector whose winner is always the higher authority.
  - `valyria-memory`: session / task / repository / user tiers in a new
    `workspace.db` block (600-699). Agent-extracted entries are `Trust::Evidence`
    and decay (confidence halves every 30 days of silence, revived on
    retrieval); user-authored entries are `Trust::Instruction` and do not.
    Retrieval scores term overlap × decayed confidence and pins session memory
    to the header; `retire` / `purge` back the eventual `valyria clean --memory`.

- **Phase 5 — search.** One query, several ways of answering it, one ranked and
  explained result.
  - `valyria-embed`: the embedding half of semantic search. An `Embedder` trait
    that `valyria-model` will implement once a real model is loaded, plus a
    deterministic `HashingEmbedder` that runs offline so semantic search works
    on a machine that never downloads a model. A generational vector store — the
    same D8 model as the index, with a rebuild reusing the vectors of unchanged
    chunks by content hash — and a compact, seeded `Hnsw` index checked against
    brute-force cosine so approximate search cannot be subtly wrong without a
    test noticing. Migration block 500-599.
  - `valyria-search`: seven independent retrieval modes — lexical (TF-IDF
    weighted content scan plus the symbol FTS), regex, symbol, semantic, AST
    (tree-sitter query patterns), dependency (graph traversal from the task's
    anchor files) and git (recent history) — combined by reciprocal-rank
    fusion and a task-aware feature reranker (recency, churn, import-graph
    distance, test proximity, a path prior). A mode with nothing to contribute
    steps aside with a note; **search works fully with embeddings disabled**.
    Every hit carries a `ScoreExplanation` whose features sum exactly to the
    hit's score, so "why this file?" is answered from stored data and the
    number cannot drift from its own explanation. A labeled-retrieval-set test
    guards ranking quality by recall@5 and MRR.
- **Phase 4 — repository intelligence.** The runtime now understands the code it
  is editing rather than only its bytes.
  - `valyria-lang`: the `LanguageProvider` trait, tree-sitter grammars and a
    declarative `.scm` query set per language, with one extraction engine driven
    entirely by capture names. Rust, Python, Go, Java, JavaScript, TypeScript
    and TSX, each behind its own cargo feature. Produces symbols with qualified
    paths, imports, call sites, tests, doc comments and signatures, plus a
    syntax-aware chunker for embeddings.
  - `valyria-index`: a **generational** file/symbol index over SQLite. Every row
    records the generation range it was valid for, so a read at generation *N*
    sees the repository exactly as it was then however far the index has moved
    on (D8). Parallel bootstrap that publishes a files-only generation first so
    a large repository is searchable before symbol extraction finishes, an
    incremental pipeline, a `resync` path for bulk changes like a branch switch,
    FTS5 symbol search, and `verify_index` — an independent rebuild diffed
    against the stored index, because index drift has no symptom of its own.
  - `valyria-graph`: the typed knowledge graph — files, modules, symbols and
    tests as nodes; contains, defines, imports, calls and tests as edges — with
    `neighbors`, `paths`, `subgraph_around` and `impact_of`. Edges carry a
    confidence, and references that leave the repository are recorded rather
    than discarded.
  - `valyria-lsp`: an LSP client, lifecycle and capped server pool. Enrichment,
    never a dependency: every entry point degrades to an empty answer when a
    server is missing, slow, or crashed, and each result is tagged with whether
    it came from the index or a language server.
  - `valyria-edit`: the last two rungs of the strategy ladder — symbol-aware
    replacement and typed AST transformation (rename, delete, insert, and
    query-driven replacement) — plus a re-parse guard that now refuses *any*
    edit that introduces syntax errors into a file that parsed cleanly.
- **Phase 3 — walking skeleton.** `valyria run "<objective>"` drives a complete
  agent loop against a real repository: the fourteen-state machine and its step
  driver (`valyria-agent`), the task manager and append-only journal
  (`valyria-task`), the `ModelRuntime` trait (`valyria-model`) with a
  deterministic scripted adapter (`valyria-runtime-fake`), minimal role routing
  (`valyria-orchestrator`), explicit-file context assembly (`valyria-context`),
  the wire protocol (`valyria-protocol`), the embedded composition root
  (`valyria-app`), and the `valyria` CLI as a thin protocol client.
  Tasks persist, stream events with a resumable cursor, survive `kill -9` and
  resume, and can be paused, resumed and cancelled from a separate process.
- **Phase 2 — execution layer.** Permission modes, categories, rule evaluation,
  scoped grants, argv-level command risk classification and unforgeable
  `Authorization` minting (`valyria-permissions`); the `Tool` trait, registry,
  invocation records and 16 first-class tools (`valyria-tools`); the editing
  engine with exact-replacement, unified-diff and whole-file strategies plus
  transactional apply and intent verification (`valyria-edit`); and the change
  ledger with external-modification detection and rollback (`valyria-ledger`).
- **Phase 1 — platform layer.** Workspace-rooted filesystem with traversal and
  symlink-escape defense, atomic writes and cached content hashing
  (`valyria-vfs`); process spawn and supervision with allowlist-first
  environment construction, output caps, timeouts and process-group kill
  (`valyria-process`); `ProcessLauncher`/`FsGuard` traits with a macOS seatbelt
  implementation, a permissive fallback and explicit confinement reporting
  (`valyria-sandbox`); hardware probing (`valyria-hardware`); and `gix`-backed
  git reads (`valyria-git`).
- **Phase 0 — foundations.** Typed ULID IDs, the trust lattice, evidence types
  and the policy vocabulary (`valyria-types`); cancellation, hashing, redaction
  and tracing setup (`valyria-util`); the single-writer SQLite actor, migrations
  and content-addressed blob store (`valyria-store`); the sequenced event bus
  with cursor resume (`valyria-events`); layered configuration with origin
  tracking and a compiled-in policy floor (`valyria-config`); the test harness
  (`valyria-testkit`); and `xtask` with the crate-layering check.
- CI: `fmt`, `clippy -D warnings`, the layering check, `cargo-deny`, an offline
  test run, an MSRV build, and the test suite on macOS, Linux and Windows.
- Documentation: `README.md`, `CONTRIBUTING.md`, `SECURITY.md`,
  `CODE_OF_CONDUCT.md`, `docs/ARCHITECTURE.md`, `docs/ROADMAP.md`, alongside the
  existing `docs/PLAN.md`.

### Known limitations

Tracked in [docs/ROADMAP.md](docs/ROADMAP.md#known-gaps). In short: no real
model runtime; the `search` and `symbol_search` tools and the agent loop are not
yet wired to the search engine (Phase 6); no planning, memory or verification;
and no sandbox confinement outside macOS.

[Unreleased]: https://github.com/adikeshri/valyria/commits/main
