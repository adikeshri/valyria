# Contributing

Thanks for looking at Valyria. This document covers the setup, the conventions
the codebase actually holds itself to, and what CI will check.

Start with [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the shape of the
system and [docs/PLAN.md](docs/PLAN.md) for the reasoning behind it. Both are
load-bearing: most review comments on structural changes reduce to "which of
these decisions does this contradict?"

---

## Setup

```bash
git clone https://github.com/adikeshri/valyria.git
cd valyria
cargo build --workspace
cargo test --workspace
```

The toolchain is pinned in [rust-toolchain.toml](rust-toolchain.toml)
(`1.97.1`, with `rustfmt` and `clippy`). `rustup` installs it on first build —
do not build with a different toolchain and do not bump the pin as a side effect
of another change.

You also need `git` on `PATH`; several test fixtures build real repositories.

## The loop

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- check-layering
```

Run all four before pushing. CI runs them plus `cargo-deny`, an offline test
run, an MSRV build, and the test suite on macOS, Linux and Windows.

## Working on a phase

Work follows the phase sequence in [docs/ROADMAP.md](docs/ROADMAP.md). Each
phase ends in a demoable capability with tests, so the tree is never in a
"nothing runs yet" state. If you are picking something up:

- Read the phase's entry in [docs/PLAN.md §5](docs/PLAN.md#5-build-phases) and
  its subsystem design in §4.
- Fill in scaffolded crates rather than adding new ones where possible. A
  scaffolded crate already has its layer, phase and CI wiring.
- Update [docs/ROADMAP.md](docs/ROADMAP.md) in the same change. A phase that
  moves from `scaffolded` to `done` without its status changing is a bug in the
  docs.

---

## The layering rule

A crate may depend only on crates in a **strictly lower** layer. This is
enforced, not suggested:

```bash
cargo run -p xtask -- check-layering
```

`xtask` reads each crate's declared tier and its `[dependencies]`, and fails on
any upward edge or same-layer cycle. `[dev-dependencies]` are exempt, which is
how any layer can use `valyria-testkit`.

Every crate declares its tier:

```toml
[package.metadata.valyria]
layer = 3
phase = 2
```

If your change needs an upward dependency, the design is wrong somewhere. The
usual fixes are: move the shared type down into `valyria-types`, invert the
dependency with a trait defined in the lower crate, or do the wiring in
`valyria-app` (the composition root — the one crate allowed to know about every
layer at once).

### Adding a crate

1. Create `crates/valyria-<name>/` with a `Cargo.toml` declaring
   `[package.metadata.valyria] layer` and `phase`.
2. Add it to `members` **and** to `[workspace.dependencies]` in the root
   `Cargo.toml`.
3. Give `src/lib.rs` a module doc comment stating the crate's layer and
   responsibility, plus `#![forbid(unsafe_code)]`.
4. Run the layering check.

Take dependency versions from `[workspace.dependencies]`
(`serde = { workspace = true }`), never pin a second version locally.

---

## Code conventions

**Errors.** `thiserror` per crate; no `anyhow` below layer 6. Every error
carries a stable `code: &'static str` and a `retryable: bool`. Errors that can
reach the model go through redaction first.

**Async.** Pure logic — state machines, ranking, parsing, planning, budget
allocation — is synchronous and unit-testable. `async` belongs in drivers and
adapters. CPU-heavy work goes to `rayon` behind a `spawn_blocking` boundary with
an explicit concurrency cap.

**Cancellation.** Every long-running operation takes a `CancellationToken` and
has a test that cancels it mid-flight and asserts cleanup. "It probably stops"
is not a guarantee; no orphan process groups may survive a cancelled task.

**IDs.** ULID-based typed newtypes, prefixed on display (`task_01H…`). Do not
pass bare `String` ids across a crate boundary.

**Serde.** Persisted and wire types use explicit versioned enums
(`#[serde(tag = "v")]`), and `deny_unknown_fields` on read paths where
strictness matters.

**Determinism.** `Clock`, `Rng` and `IdGen` are injected traits. Never call
`SystemTime::now()` or `rand::random()` directly in library code — journal
replay tests depend on this.

**Unsafe.** `#![forbid(unsafe_code)]` at the top of every crate. Removing it
needs a written justification in the PR.

**Comments.** Explain *why*, especially where the code encodes one of the
design decisions (D1–D12). The existing comments are dense on purpose: they are
how a reader learns that a `flush()` exists because a crash-recovery test reads
that line, or that a scenario file and a fixture are a matched pair. Match that
density; do not narrate what the code plainly says.

**Never silently degrade.** If a capability is unavailable — sandbox
confinement, an LSP server, embeddings — report the actual level to the client
and carry on explicitly. Reduced function that looks like full function is
treated as a bug.

---

## Tests

New code needs tests in the layer that suits it:

| Kind | For |
|---|---|
| Unit | Pure logic: state machine, parsers, ranking, budget allocation, plan validation |
| Property / fuzz | Patch and diff parsers, protocol decoding, path resolution, tool inputs |
| Tool tests | Each tool against a fixture workspace, including failure and permission-denied paths |
| Sandbox tests | Escape attempts, per platform; must fail closed |
| Scenario tests | Agent behaviour, driven by `valyria-runtime-fake` scripts — the bulk of orchestration coverage |
| Journal replay | Replaying a recorded task produces identical state |
| End-to-end | The compiled binary against a real git fixture repo |

Guidance:

- Prefer a fake-model scenario over a mock. The fake runtime is first-class
  infrastructure (D12), not test scaffolding, and it can script malformed
  output, tool-call storms and refusals.
- A permission-relevant change needs a denied-path test, not only an allowed one.
- Tests must pass offline. CI runs the whole suite with networking disabled.
- Integration realism beats coverage percentage on adapter crates; logic crates
  are held to coverage gates.

---

## Pull requests

- One coherent change per PR. Cross-layer refactors and feature work in the same
  PR are hard to review against the layering rule.
- Say which phase and which design decision the change relates to. "Phase 4,
  extends D8's generational reads" tells a reviewer more than a diff summary.
- Update the docs in the same PR: `docs/ROADMAP.md` for status, the crate's
  module doc for responsibility changes, `docs/ARCHITECTURE.md` if the topology
  moved, `CHANGELOG.md` under `Unreleased`.
- Green CI is a prerequisite, not the review.

Commit messages: imperative subject under ~72 characters, body explaining why
where it isn't obvious.

## Security-relevant changes

Anything touching `valyria-permissions`, `valyria-sandbox`, `valyria-process`,
credential scrubbing, or prompt assembly gets extra scrutiny. Read
[SECURITY.md](SECURITY.md) first, and do not report a vulnerability in a PR or a
public issue — the disclosure process is described there.

## Code of conduct

Participation is governed by [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md).
