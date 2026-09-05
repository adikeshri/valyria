# Valyria

A local-first coding agent runtime, written in Rust. Valyria plans, edits, runs,
and verifies changes inside a real Git repository using models that run on your
own machine — no cloud service, no telemetry, and no network at runtime.

## Status

Pre-1.0 and under active development. The agent loop runs end to end today
against a deterministic **fake** model; the real model runtimes (llama.cpp, MLX)
and the OpenAI-compatible HTTP transport are scaffolds. The wire protocol is a
frozen v1 (`1.11.0`); CI rejects any change that does not also bump
`PROTOCOL_VERSION`.

See [docs/ROADMAP.md](docs/ROADMAP.md) for per-subsystem status and
[CHANGELOG.md](CHANGELOG.md) for what has landed.

## Requirements

- Rust `1.97.1`, pinned in [rust-toolchain.toml](rust-toolchain.toml)
- `git` on `PATH`
- Linux or macOS. Tier 1: macOS aarch64, Linux x86_64. Windows builds and runs
  the daemon over a named pipe, but has no access sandbox yet.

## Build

```bash
cargo build --workspace
```

## Run

Drive a task against a throwaway Git repository — the agent really does edit
files:

```bash
cargo run -p valyria-cli -- run "add a function" --workspace /path/to/repo --events
```

`--events` streams every protocol event as JSON. `--plan` runs the objective as
a validated, checkpointed plan rather than a single task.

### Commands

```
valyria run "<objective>" [--workspace <path>] [--permission-mode MODE] [--plan] [--events]
valyria task <status|list|report|plan|rollback|pause|resume|cancel|permission> <id>
valyria serve [--socket <path>] [--auth-token-file <path>]   # daemon; one per workspace
valyria doctor | clean | status | config | model list | memory list
valyria                                                      # interactive TUI
valyria --connect <socket> <command>                         # run a command against a daemon
```

### Permission modes

| Mode | Behaviour |
|---|---|
| `manual` | Ask before every mutating action. |
| `assisted` (default) | Auto-allow reads and workspace-scoped safe commands; ask for everything else. |
| `autonomous` | Auto-allow inside the workspace and plan scope; still ask for destructive, network, and out-of-workspace actions. |

A task awaiting approval parks in `WAITING_FOR_PERMISSION`; resolve it with
`valyria task permission resolve <id> --allow`.

## State

```
<repo>/.valyria/     per-workspace, gitignored: workspace.db (tasks, journal,
                     ledger, evidence, index metadata), index/, cache/, tasks/
~/.valyria/          global: config.toml, global.db, models/, logs/
```

Deleting `<repo>/.valyria` discards all agent state for that repository.

## Development

```bash
cargo test --workspace
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p xtask -- check-layering    # the crate graph is strictly layered
cargo run -p xtask -- release-gates     # layering, protocol compat, benchmarks, acceptance
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for conventions and what CI enforces.

## Documentation

| Document | Contents |
|---|---|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Crate topology, design decisions, task lifecycle |
| [docs/PLAN.md](docs/PLAN.md) | Full build plan: subsystem designs, phases, acceptance criteria |
| [docs/ROADMAP.md](docs/ROADMAP.md) | Per-subsystem status |
| [docs/BENCHMARKS.md](docs/BENCHMARKS.md) | The `valyria-bench` evaluation harness and its CI gate |
| [SECURITY.md](SECURITY.md) | Threat model, sandboxing guarantees, vulnerability reporting |

## License

Apache-2.0. See [LICENSE](LICENSE).
