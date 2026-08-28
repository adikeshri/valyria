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

Last updated: 2026-08-28 (after Phase 6).

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
| 7 | Verification, diagnosis, repair: `verify`, failure parsers, repair loop, loop detection | scaffolded |
| 8 | Planning and multi-agent: `plan`, checkpoints, rollback boundaries, sub-tasks | scaffolded |
| 9 | Real models: `runtime-llamacpp`, `runtime-mlx`, `runtime-openai-compat`, registry, store, model pool | scaffolded |
| 10 | Interface completion: protocol v1 freeze, schema export, full CLI, TUI, `doctor`, `clean`, daemon | not started |
| 11 | Hardening and evaluation: `bench`, fuzzing, perf work, cross-platform matrix, release gates | not started |

Phase 7 is parallelizable now that 4 has landed. Phase 9 can start early against
the OpenAI-compatible adapter (a locally running `llama-server`) without waiting
for any FFI work.

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
| Only the fake model runtime exists; no real inference | Phase 9 |
| No verification or repair loop; no planning | Phases 7–8 |
| No `doctor`, `clean`, storage inspection, daemon mode, or TUI | Phase 10 |
| Protocol is unversioned in practice — no schema export or compat gate yet | Phase 10 |

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
