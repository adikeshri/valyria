# Security

Valyria runs untrusted model output against a real repository and a real shell.
Security is not a feature area here; it is most of the design. This document
states what is guaranteed today, what is not yet, and how to report a problem.

**Valyria is pre-1.0 and phases 4–11 are unbuilt. Do not run it against
repositories or machines you cannot afford to have damaged, and read
[Current guarantees](#current-guarantees) before running it in any mode other
than `manual`.**

---

## Reporting a vulnerability

Report privately through GitHub's private vulnerability reporting on
<https://github.com/adikeshri/valyria/security/advisories/new>. Please do not
open a public issue, and do not include a proof of concept in a pull request.

Include: what you can make the runtime do, the mode and platform, and a minimal
reproduction if you have one. You should get an acknowledgement within a few
days; because this is a pre-1.0 project maintained in the open, fixes ship as
ordinary commits with the advisory published once a fix is in `main`.

Findings that are especially valuable: anything that mints or bypasses an
`Authorization`, escapes the workspace root, escapes the sandbox, leaks
credentials into a model prompt or a log, or gets repository content promoted to
instruction trust.

---

## Threat model

The adversary is assumed to be **the content the agent reads**: model output, a
malicious repository, a poisoned dependency's README, a crafted test failure. A
user who runs the binary is trusted; everything the binary reads is not.

The properties the design aims to hold:

1. **No tool executes without an unforgeable capability.** `Tool::execute` takes
   an `Authorization` whose constructor is private to `valyria-permissions`, and
   which is bound to `(task_id, step_id, tool, canonical_input_hash, expiry)`.
   Neither a model response, nor a tool, nor the agent crate can mint one. The
   input-hash binding prevents TOCTOU substitution: an approval for `rm ./tmp`
   cannot be spent on `rm -rf /`.
2. **Repository content can never become an instruction.** Context items carry a
   trust level (`Policy > Instruction > Evidence > RepoData > ModelOutput`) and
   prompt assembly — the single place that builds a prompt — refuses to place
   anything below `Instruction` in a system or policy position. Everything at
   `Evidence` or below is nonce-fenced and framed as data.
3. **Model claims are never evidence.** Completion reports are generated only
   from `Evidence` rows, which models cannot construct. An unverified claim
   reports as "not verified".
4. **Config cannot exceed the policy floor.** Permission and network policy are
   configurable, but validated against a compiled-in floor; configuration cannot
   grant something the floor forbids.
5. **Credentials are stripped by default.** Child process environments are built
   allowlist-first; `*_TOKEN`, `*_KEY`, `AWS_*`, `SSH_AUTH_SOCK` and friends are
   removed unless explicitly permitted.
6. **Nothing degrades silently.** When confinement is weaker than intended, the
   runtime reports the level it actually achieved (`none`, `filesystem`,
   `filesystem+network`, `filesystem+network+resource`) rather than pretending.
7. **The agent cannot clobber your work.** Writes carry the content hash the
   agent believes the file has; a mismatch fails as `ExternalModification` and
   becomes an observation, not an overwrite.
8. **No network at runtime.** Internet access and credential access are `Denied`
   by default in the network policy. A CI job runs the full test suite with
   networking disabled.

Out of scope: protecting against a user who deliberately configures
`autonomous` mode with a permissive sandbox and points the agent at their home
directory; supply-chain compromise of the Rust dependency tree (mitigated only
by `cargo-deny` advisories); and the security of any model weights you choose to
run.

---

## Current guarantees

Status as of Phase 3. Read this as the honest version of the section above.

| Property | Status |
|---|---|
| `Authorization` capability gate on every tool call | implemented |
| Permission modes, categories, rule evaluation, policy floor | implemented |
| Workspace-rooted path resolution, symlink escape refusal | implemented |
| Allowlist-first environment construction, credential stripping | implemented |
| Process-group kill, output caps, wall-clock timeouts | implemented |
| Optimistic-concurrency writes and the change ledger | implemented |
| Sandbox confinement on **macOS** (seatbelt) | implemented |
| Sandbox confinement on **Linux** (namespaces / seccomp / landlock) | **not implemented** — falls back to `PermissiveSandbox` (`Confinement::None`) |
| Sandbox confinement on **Windows** (job objects / AppContainer) | **not implemented** — falls back to `PermissiveSandbox` |
| Trust-lattice prompt assembly and injection defense | **not implemented** — Phase 6 |
| Evidence-only completion reports | types exist; enforcement lands with Phase 7 |
| Command risk classification (argv-level, dangerous-form database) | partial |
| Fuzzing of patch/diff/protocol parsers | **not implemented** — Phase 11 |

The practical consequence: **on Linux and Windows today, a command the
permission engine allows runs with no OS-level confinement.** The runtime
reports this, but it means `autonomous` mode on those platforms is currently
only as safe as the permission rules themselves.

---

## Running it safely today

- Use `manual` or `assisted` mode. `autonomous` is for when Phases 4–11 have
  landed and you trust the sandbox on your platform.
- Point `--workspace` at a repository with committed work, or a scratch clone.
  Rollback exists but is not yet a substitute for a clean git state.
- On Linux and Windows, treat every approved command as if it were run in your
  own shell, because it effectively is.
- Delete `<repo>/.valyria` to discard all agent state, including journals and
  stored transcripts, for that repository.

## Supported versions

Pre-1.0: only `main` receives fixes. There are no released versions to backport
to yet.
