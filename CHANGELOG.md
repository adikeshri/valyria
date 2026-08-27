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
model runtime, no index or search, no planning, memory or verification, and no
sandbox confinement outside macOS.

[Unreleased]: https://github.com/adikeshri/valyria/commits/main
