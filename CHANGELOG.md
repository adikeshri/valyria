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

- **Protocol 1.10.0 — `index_build`** (additive; minor bump). A new
  `Request::IndexBuild` / `Response::IndexBuild` (same shape as
  `IndexStatus`) that runs `Runtime::reindex` — the whole-workspace index +
  graph build — synchronously and returns the finished generation. The
  desktop client's "Build index" action calls it so `search_query` /
  `index_status` have something to serve on a fresh workspace. No new
  capability; part of the existing `repo` surface.

- **Protocol 1.9.0 — Windows named-pipe transport** (desktop-client gap
  closure G9; additive; runtime capabilities `daemon`, `windows`).
  - `SocketClient` and `daemon::serve` now speak a **Windows named pipe**
    (`\\.\pipe\valyria-<id>`) as well as a Unix-domain socket, behind the
    same `Client` trait — `valyria serve` and `--connect` work on Windows.
    The frame handling is shared (`daemon::framed::serve_connection`);
    only the listener and the peer check differ. `SocketClient::new`
    accepts either a socket path or a pipe name.
  - The peer boundary on Windows is the pipe's default ACL (creating
    user's token — the `SO_PEERCRED` analogue); the per-frame `auth_token`
    (G10) applies identically.
  - `hello` now advertises `daemon` where the IPC transport exists and
    `windows` on a Windows build (both runtime-conditional, not in
    `capability::ALL`).
  - The whole workspace cross-compiles for `x86_64-pc-windows-*`; the
    named-pipe daemon test runs on the `windows-latest` CI matrix.
  - **Not** in scope: a Windows *access* sandbox (Job Objects / restricted
    tokens). `detect_platform_launcher` still returns `PermissiveSandbox`
    on Windows — `Confinement::None`, surfaced honestly by `doctor_run`.
    Tracked as follow-up (needs a Windows CI runner to verify).

- **Protocol 1.8.0 — approval identity & scope** (desktop-client gap
  closure G2; additive, backward compatible; capability `approval_scope`).
  - `approval_requested` now carries a stable `request_id` (the pending
    tool call's effect id).
  - `permission_resolve` gains `request_id?` and `decision`
    (`once` | `task` | `deny`). When `request_id` is set it is asserted
    against the daemon's current pending request — a stale prompt is
    refused with `approval.superseded` rather than resolving the wrong
    call. `task` is "Allow for Task" (`GrantScope::Task`), so the same
    class of request auto-allows for the rest of that task. `approve` is
    kept for 1.0 clients and used only when `decision` is absent.
  - New `AgentDriver::resolve_permission_scoped` /
    `Runtime::resolve_permission_scoped` /
    `valyria_agent::ApprovalDecision`. `AppError::Agent` now reports the
    inner code, so `approval.superseded` reaches the wire.

- **Protocol 1.7.1 — event payload contracts** (desktop-client gap closure
  G12; docs/gate only, patch bump).
  - `docs/protocol/event-kinds.txt` — the canonical event-`kind` list,
    exported from `valyria_events::EventKind` and gated against drift by
    `xtask check-protocol` (a new kind without a contract fails CI).
  - `docs/protocol/events/<kind>.schema.json` — a JSON Schema per kind
    with a pinned payload shape (`state_changed`, `tool_started`,
    `tool_completed`, `approval_requested`, `context_retrieved`,
    `plan_checkpoint`, `model_install_*`, `verification_evidence` /
    `test_failed`), from new mirror structs in
    `valyria_protocol::event_payloads`. Kinds without a struct keep an
    intentionally open payload. Regenerated and gated with the request /
    response schemas.

- **Protocol 1.7.0 — per-task event filter** (desktop-client gap closure
  G11; additive, backward compatible; capability `stream_filter`).
  - The subscribe frame (`ClientFrame::Subscribe` / `AuthSubscribe` and
    `EventsSubscribeRequest`) gains an optional `task_id`. When set the
    stream carries that task's events plus workspace-global (task-less)
    ones only, so a per-task subscriber is not fed every other task's
    activity. Omitted = the full stream, unchanged.
  - New `Client::subscribe_events_for_task(since, task_id)` with a
    filter-ignoring default; the embedded and socket transports override
    it. `subscribe_events` is unchanged.
  - The async-method half of G11 (long operations return immediately and
    report via events) already shipped with `model_install` in 1.3.0.

- **Protocol 1.6.0 — local client authentication** (desktop-client gap
  closure G10; additive; capability `client_auth`).
  - Every daemon connection is now **peer-uid checked**: only the OS user
    that started the daemon may connect (`UnixStream::peer_cred`), else
    `auth.peer_uid`. Needs no wire change and applies to every frame.
  - `daemon::serve` gains an `auth_token: Option<String>` parameter. When
    set, clients must send the new `ClientFrame::AuthCall` /
    `AuthSubscribe` variants carrying the token; a bare `Call` / `Subscribe`
    is refused with `auth.required`, a wrong token with
    `auth.token_mismatch`. `SocketClient::with_token` produces an
    authenticating client. `valyria serve` / the CLI gain
    `--auth-token-file <path>`.
  - `EmbeddedClient` (in-process) is unaffected — it is already inside the
    trust boundary.

- **Protocol 1.5.0 — diagnostics granularity** (desktop-client gap closure
  G13, G14, G15; additive, backward compatible; capability
  `diagnostics_v2`).
  - **G13** — `PlanStepSummary` gains an optional `checkpoint_id` (the id
    `task_rollback` expects, joined in from `plan_checkpoint` rows), and a
    `plan_checkpoint` event `{ checkpoint_id, step_id }` is now projected
    from the `CONTEXT_RETRIEVED`-style journal entry so a client can learn
    an id live. New `Runtime::plan_checkpoints`.
  - **G14** — `tool_started` now carries a `tool_invocation_id` that
    matches the one on `tool_completed` (the effect id, the real pairing
    key). `tool_completed` gains structured `exit_code`, `stdout`,
    `stderr`, `duration_ms` (from the `ToolInvocationRecord`) alongside
    the pre-formatted `rendered` blob, plus `tool_record_id`.
  - **G15** — `verification_evidence` / `test_failed` gain a
    `failures: [{ kind, message, failing_test, location: [{ path, line }] }]`
    array built from `valyria-verify`'s parsed `Failure`s, so a test
    failure can open its parsed location.

- **Protocol 1.4.0 — context provenance & change ownership** (desktop-client
  gap closure G7, G8; additive, backward compatible; capabilities
  `context`, `ledger`).
  - `context_retrieved` **event** — the agent driver now records what the
    context assembler retrieved for each Discovery step as a journal
    entry (`kinds::CONTEXT_RETRIEVED`), which `TaskManager` projects to a
    `context_retrieved` event: `{ items: [{ path, reason, trust_level,
    tokens, score }], budget_used, budget_total }`. The Context Inspector
    has a data source (§34).
  - `ledger_changes { task_id }` — one row per agent-touched file with
    `valyria-ledger`'s classification (`agent_authored` / `pre_existing` /
    `concurrent_user_modification` / `unknown`), computed against the
    file's on-disk state now, plus `kind` (write/delete), `step_id` and
    `tool_invocation_id`. The diff viewer's ownership column stops reading
    "unavailable" (§15, §16).

- **Protocol 1.3.0 — hardware probe & model management** (desktop-client
  gap closure G4, G5; additive, backward compatible; capabilities
  `hardware`, `model_manage`).
  - `hardware_probe` — the full `valyria_hardware::HardwareReport` (CPU,
    RAM, GPUs, unified-memory / accelerator flags, disk) on the wire, so
    the first-run wizard has a structured source instead of prose.
  - `model_recommend { role }` — every catalog candidate scored against
    measured hardware with Core's `fit()` (`valyria_model_registry::
    score_card_for_role`): `fit_kind` (comfortable / tight / will_not_fit),
    `fit_detail`, `suitability`, `adjusted_score`, `installed`. The
    recommendation is Core's, not an app heuristic (§41). Non-fitting
    candidates are still listed, sorted last.
  - `model_install { id }` — returns immediately; the resumable, verified
    download runs on a background task and reports
    `model_install_progress { id, phase, downloaded_bytes, total_bytes }`
    then `model_install_completed { id, size_bytes }` or
    `model_install_failed { id, code, message }` on the event stream (three
    new `EventKind`s). `model_remove { id }` → freed bytes;
    `model_activate { id, role }` binds a role in `global.db`;
    `model_inspect { id }` → card + manifest + active roles.
  - New `valyria-model-store` surface: `ModelStore::install_with_progress`
    (progress callback), `InstalledModelStore` role-binding table
    (migration 901), and `HttpFetcher` — a `reqwest` + `rustls` `Fetcher`
    implementation behind the default `http` feature (the first HTTP
    client in the workspace; `--no-default-features` drops the TLS stack).
    `deny.toml` gains `CDLA-Permissive-2.0` for `webpki-roots`.
  - The post-install probe is still `NullProber` — a real GGUF load probe
    needs a linked inference runtime (tracked separately).

- **Protocol 1.2.0 — repository read surface** (desktop-client gap closure
  G3; additive, backward compatible; capability `repo`).
  - `git_status` — branch / detached / HEAD SHA plus per-file
    staged/unstaged/untracked entries.
  - `git_diff { path?, staged? }` — unified-diff *text* for the working
    tree (`staged=false` → worktree vs index, `staged=true` → index vs
    HEAD), path-filterable, capped at 512 KiB with a `truncated` flag.
    Backed by a new `valyria_git::Repo::worktree_diff` built on `gix`
    blob reads + `imara-diff`'s unified formatter — no shelling to `git`.
  - `git_log { limit }` — newest-first commits from HEAD (capped 500;
    unborn HEAD yields an empty list). `git_branches` — local branches
    with the HEAD marker.
  - `search_query { query, modes[], anchors[], limit }` — the fused
    seven-mode code search (`valyria-search`), returning ranked
    `SearchHit`s each with the full `ScoreExplanation` (stage scores,
    weighted features that sum exactly to the score, retrieval path) and
    the `modes_run` / `degraded` notes. An unknown mode is
    `search.unknown_mode`; an un-indexed workspace is `search.not_indexed`.
  - `index_status` — current generation, stage, file/symbol counts.
  - `Runtime::reindex` — explicit whole-workspace index + graph build, the
    entry point for the client's "build index" action and first-run.

- **Protocol 1.1.0 — per-task autonomy and config writes** (desktop-client
  gap closure G1, G6; additive, backward compatible).
  - `task_create` gains an optional `permission_mode` (`manual` |
    `assisted` | `autonomous`). A task created with it runs at that
    autonomy level regardless of the daemon's start-time mode, so a client
    can offer a Manual/Assisted/Autonomous switch without restarting the
    workspace daemon (§25). `PermissionEngine` now carries per-task mode
    overrides (`set_task_mode` / `clear_task_mode` / `effective_mode`),
    resolved per decision and released when the task terminates. Omitting
    the field is exactly the old behaviour.
  - `config_set { key, value, scope }` writes one dotted leaf to a
    Core-owned file (`workspace` → `<repo>/.valyria/config.toml`, `user` →
    `~/.valyria/config.toml`) and returns the re-resolved `config_show`
    view. The write is policy-floor validated before it touches disk — a
    value that would loosen access past the compiled ceiling is refused
    with `config.policy_floor_violation` and nothing changes — and is
    atomic (temp file + rename). New `valyria_config::write_key` /
    `WRITABLE_KEYS`. `config_show` now reports the `network` policy as its
    five individual leaves (`network.internet`, …) rather than one debug
    blob, so a write and a re-read line up.
  - New `hello` capabilities: `config_write`, `task_permission_mode`.

- **Phase 11 — hardening and evaluation.** The runtime now grades itself:
  an executable-oracle benchmark harness, an offline fixture suite that is
  a CI regression gate, property/fuzz suites for the parser surfaces, and
  a machine-checked acceptance mapping.
  - `valyria-bench` (was a scaffold, now implemented): `BenchTask =
    { repo: RepoSpec, objective, scenario, oracle }`. `Oracle` is an
    **executable** check — `CommandSucceeds { program, args }` (runs a
    real command in the finished workspace, "the tests pass"),
    `ReportVerified` (completion-report status, from durable runs only —
    D4), `TaskCompleted`, `FileContains` / `FileLacks` / `FileExists`,
    `MaxFilesChanged`, `PathsUntouched`, `All(...)`. `BenchRunner::run`
    materializes the repo, opens a real `valyria_app::Runtime` bound to
    the fake-model `Scenario`, drives the task to a terminal state, diffs
    the on-disk tree for the changed-file set, projects the journal into
    `BenchMetrics`, and grades — fully hermetic (throwaway workspace *and*
    `~/.valyria`), offline. `fixture_suite()` is one task per §4.30
    category (feature, verified bug fix, a Diagnosing→Repairing
    `debugging_repair_loop`, refactor rename, test creation, dependency
    edit, zero-change exploration); all seven pass against the real
    runtime with the network down. `BenchReport` is serializable;
    `compare(baseline, current)` flags task regressions and cost-metric
    blow-ups. New `valyria-bench` binary (`run` / `baseline`) and a
    `perf` module for the §9 runtime-only budgets (`#[ignore]`d).
  - `xtask`: `bench [--bless]` (run the suite, diff against
    `docs/bench/baseline.json`, fail on regression; `--bless` re-records)
    and `release-gates` (layering + protocol compat + benchmark baseline
    + the acceptance doc, one summarised pass). New CI jobs `bench`,
    `property` (proptest suites at `PROPTEST_CASES=2048`) and
    `release-gates` in `ci.yml`.
  - Property / fuzz suites (§7): `valyria-edit/tests/fuzz_edit.rs` (the
    exact-replacement and unified-diff / `diffy` parsers are total — any
    input is `Ok` or a typed `Err`, never a panic — plus a real
    single-hunk patch round-trips); `valyria-protocol/tests/fuzz_protocol.rs`
    (arbitrary bytes never panic the frame decoder; every constructible
    `Request` survives `encode_line` → `from_str`);
    `valyria-tools/tests/fuzz_tool_inputs.rs` (D2 `canonical_input_hash`
    is total, deterministic and key-order-independent — the property the
    TOCTOU guarantee rests on).
  - `docs/ACCEPTANCE.md` maps all 18 PLAN §6 criteria to a proving test;
    `crates/valyria-bench/tests/acceptance.rs` asserts each is
    demonstrated by the fixture suite or proven elsewhere, with one
    documented deferral (the suite against a *real* local model).
    `docs/BENCHMARKS.md` documents the harness.
  - Deliberate scope: the suite runs against the deterministic fake model
    (a real-`llama-server` run, a pinned-real-repo corpus for the scale
    perf budgets, a SWE-bench adapter, `cargo-fuzz` nightly targets, and
    real Linux/Windows sandbox confinement are follow-ups). 29 new tests;
    1087 pass across the workspace.

- **Phase 10 — interface completion.** The runtime is now drivable end to end
  through one frozen protocol — embedded, over a Unix-socket daemon, from a full
  CLI, or from an interactive TUI — plus `doctor`, `clean`, and the `global.db`
  assembly point.
  - `valyria-protocol`: the wire surface is a **frozen v1** (`PROTOCOL_VERSION =
    1.0.0`). `Request`/`Response` gained `task_list`, `task_report`, `task_plan`,
    `task_rollback`, `workspace_status`, `doctor_run`, `storage_inspect`,
    `storage_purge`, `config_show`, `memory_list`, `model_list`; `HelloResponse`
    now advertises `capabilities`. Every wire type derives `schemars::JsonSchema`;
    `schema::export` renders `docs/protocol/{request,response,event}.schema.json`
    + `version.txt`. New `transport` module: newline-delimited JSON framing and
    `SocketClient`, the daemon-transport implementation of `Client` — a missing
    socket is a clean `protocol.transport` error, never a panic.
  - `xtask schema` writes the schema files; `xtask check-protocol` (new CI job,
    `protocol` in `ci.yml`) fails any drift in `docs/protocol/` that did not also
    bump `PROTOCOL_VERSION` — the §4.27 machine-checked compat gate.
  - `valyria-app`: `GlobalStore` opens `~/.valyria/global.db` (`$VALYRIA_HOME`
    override) — the §4.1 assembly point that concatenates the installed-model
    index (block 900-999), user-scoped memory (600-699) and a new
    `workspace_registry` (block 10_100+); every `Runtime::open` registers its
    workspace. `Doctor` (`doctor` module): ten environment checks — runtime
    build, data-dir writability, `workspace.db` `integrity_check`, disk space,
    git health, sandbox confinement, inotify watch ceiling, permission-config vs
    the policy floor, index presence, installed models — each returning
    `pass`/`warn`/`fail`, a detail, and a concrete remediation. `StorageInspector`
    (`storage` module): `inspect` sizes every on-disk area, `purge(scope,
    dry_run)` backs `valyria clean` over `memory`/`cache`/`tasks`/`logs`.
    `daemon::serve` is the Unix-socket accept loop — it dispatches each framed
    frame straight into an `EmbeddedClient`, so the socket path and the
    in-process path run *identical* runtime code (the whole point of D11).
    `Runtime` grew the read-only surface `list_tasks` / `completion_report` /
    `doctor` / `storage_inspect` / `storage_purge` / `config_show` /
    `memory_list` / `model_list` / `current_index_generation`.
  - `valyria-cli`: full subcommand tree — `run`, `task
    status|list|report|plan|rollback|pause|resume|cancel|permission`, `doctor`,
    `clean`, `status`, `config`, `model list`, `memory list`, `serve`. Global
    `--json` (machine output) and `--connect <socket>` (swap the embedded client
    for `SocketClient` — no other code path changes). `valyria` with no arguments
    opens the **TUI session** (ratatui): task list, live event log, compose a new
    objective, pause/resume/cancel and allow/deny permission on the selected
    task — all through the same `Client` trait, so it works identically against a
    daemon. Integration tests (`tests/phase10.rs`) drive the real binary for
    `doctor`, `status`, `config`, `model list`, `clean --dry-run`, and a
    `serve` + `--connect` round trip.
  - Deliberate scope: the `--connect` daemon is single-workspace (the socket
    daemon owns one `Runtime`); a multi-workspace daemon, `agent.events`
    length-prefixed framing, TypeScript type export, and `doctor`'s live sandbox
    self-test are follow-ups. The TUI is a session view, not a full editor.

- **Phase 9 — real models (offline slice).** The model layer is now real: a
  catalog, a verified download store, an OpenAI-compatible runtime adapter, and
  the tool-call transport ladder that makes unreliable open-weight models usable.
  - `valyria-model-registry` (new crate): the `ModelCard` catalog — id, family,
    quantization, context length, file size, recommended sampling, per-role
    suitability scores, `ModelRequirement`, transport preference, license, source
    URL and blake3 hash — shipped **embedded** (`catalog.json`, compiled in) so
    the runtime works offline. `ModelRole` is the full role set (PrimaryCoder,
    FastCoder, Planner, Reviewer, Embedder, Reranker, Autocomplete, Summarizer)
    with eviction priority and escalation edges. `select_for_role` scores
    `(model, role)` pairs against **measured** hardware via
    `valyria_hardware::fits`, penalising a `Tight` fit below a comfortably-
    fitting alternative; `RoleBinding::derive` builds a primary + ordered
    fallback chain.
  - `valyria-model-store` (new crate, migration block 900-999): the on-disk
    weights store. `plan_install` surfaces size + license + hardware fit and must
    be `.confirm()`ed; `install` does a **resumable** chunked download behind the
    `Fetcher` seam (`.part` file, byte-range resume), a **whole-file blake3
    integrity check** that deletes the file and hard-errors on mismatch, a probe
    behind the `Prober` seam, then an atomic `manifest.json`. `verify_integrity`,
    `remove` (reports freed bytes), `gc` (drops models not in the keep-set, sweeps
    stray partials) and `storage_report` round it out; `InstalledModelStore` is a
    rebuildable DB index over the manifests.
  - `valyria-runtime-openai-compat` (new crate): `OpenAiCompatRuntime`, a
    `ModelRuntime` for any local OpenAI-compatible server (llama-server, vLLM,
    Ollama, LM Studio). HTTP sits behind the `HttpTransport` trait, so
    `/v1/chat/completions` request building, buffered and SSE response parsing,
    native tool-call extraction, `/health`, and mid-request / mid-stream
    cancellation are all covered against a scripted `MockTransport`.
  - `valyria-orchestrator`: hardened from the Phase 3 stub. `structured` is the
    **tool-call transport ladder** (D5) — native `tool_calls` first, then a
    tolerant recovery parser over model text (strips ```` ```json ````,
    `<tool_call>` / `[TOOL_CALLS]` / `<|python_tag|>` wrappers and prose; pulls
    the first *string-aware* balanced JSON value; tolerates trailing commas;
    accepts `{name,arguments}`, `{tool,args}`, `{function:{…}}`, `{tool_call:{…}}`
    and arrays; stringified arguments) — then `resolve_tool_calls` feeds a parse
    failure back to the model as evidence for a **bounded reformat-retry**.
    `ModelPool` is memory-aware **admission control**: LRU-within-role-priority
    eviction that never displaces a higher-priority model, emitting
    `ResourcePressure` / `Evicted` / `Loaded` events. `RoleRouter` walks a
    `RoleBinding`'s fallback chain, skipping unregistered or unhealthy models and
    retrying the next on a retryable error. `Role` is now a re-export of
    `ModelRole`; the Phase 3 `Orchestrator` is unchanged.
  - Deliberate scope (open decision 5 — solo build defers MLX/CUDA): the concrete
    `reqwest` HTTP transport, the real GGUF-loading probe, and the
    `valyria-runtime-llamacpp` / `valyria-runtime-mlx` adapters remain documented
    scaffolds; the ladder, pool and router are built and tested but not yet wired
    into the live agent loop (which still drives through `Orchestrator`). 78 new
    tests; 1020 pass across the workspace.

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
