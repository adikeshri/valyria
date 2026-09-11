# Releasing

How `valyria` (Core) cuts a release: a tag push that builds the `valyria`
CLI/daemon binary for four platforms and publishes it to a GitHub Release.

This is the **only** supported way for downstream consumers — today, just
`valyria-app` — to obtain a Core binary. `valyria-app`'s own release pipeline
downloads and checksum-verifies the exact artifact named in its
`core.lock.json` `release` block; see that repo's `docs/RELEASING.md`.

## What the pipeline does

`.github/workflows/release.yml`, triggered by `push: tags: ["v*"]` (normal
case) or `workflow_dispatch` with a `tag` input (to repair/re-run a release
without re-tagging — it never creates or pushes a tag itself):

1. **`check`** — `cargo xtask check-version-tag <tag>` asserts the tag (minus
   its leading `v`) equals `[workspace.package] version` in the root
   `Cargo.toml`. Fails before any build if they disagree.
2. **`build`** — one job per platform/arch, `fail-fast: false`:
   - `aarch64-apple-darwin` and `x86_64-apple-darwin`, both on the same
     `macos-14` (Apple Silicon) runner. The x64 leg is cross-compiled
     (`rustup target add x86_64-apple-darwin`) — `valyria-runtime-mlx` is a
     pure-Rust stub with no native Metal/Obj-C linkage today, so no special
     toolchain is needed. **Revisit this once a real native MLX adapter
     lands** (Apple-Silicon-only) — that will likely need a build-time `cfg`
     gate or a stub backend for the x64 leg.
   - `x86_64-unknown-linux-gnu` on `ubuntu-22.04` (pinned, not
     `ubuntu-latest`) — an older glibc keeps the binary running on a broader
     range of Linux systems.
   - `x86_64-pc-windows-msvc` on `windows-latest`.
   - Each leg builds `-p valyria-cli --release --locked`, names the result
     `valyria-<version>-<target-triple>[.exe]`, and writes a matching
     `.sha256` file. Raw binaries, not archives — GitHub Release assets don't
     preserve the exec bit either way, so this saves the downloader an
     extraction step.
3. **`publish`** — collects all four legs' artifacts, assembles one
   `SHA256SUMS` manifest, and runs `gh release create <tag> ...`. **Not a
   draft** — the release must be plain-`curl`-fetchable, since
   `valyria-app`'s pipeline downloads directly from it. A tag containing a
   `-` (e.g. `v0.2.0-rc.1`) is published with `--prerelease` instead of a
   full release.

## Signing

**Unsigned today.** No `MACOS_SIGN_IDENTITY` or `WINDOWS_SIGN_PFX` secrets are
configured, so the codesign/notarize/signtool steps in `release.yml` are
no-ops (each is gated `if: env.<VAR> != ''`) and every release notes this
explicitly. When credentials exist, set those secrets — no workflow redesign
is needed to activate signing.

## Versioning

`valyria-cli`'s `Cargo.toml` uses `version.workspace = true`, so
`[workspace.package] version` in the root `Cargo.toml` is the single source
of truth — bump it by hand, no sync script needed (unlike `valyria-app`,
which has to keep several files in lockstep). Tag format is plain `vX.Y.Z`,
with `-rc.N` / `-alpha.N` suffixes for prereleases.

Each release should move `CHANGELOG.md`'s `## [Unreleased]` section under a
new dated heading (`## [X.Y.Z] - YYYY-MM-DD`) in the same commit as the
version bump, following the file's existing Keep a Changelog convention.

## Cutting a release

1. Bump `[workspace.package] version` in `Cargo.toml`.
2. Move `CHANGELOG.md`'s `[Unreleased]` section under `## [X.Y.Z] - <date>`.
3. Commit, `git tag vX.Y.Z`, `git push origin main --tags`.
4. Watch the Actions run (`gh run watch`). A clean run means: 4 green
   `build` legs, then `publish` producing a non-draft release at
   `github.com/adikeshri/valyria/releases/tag/vX.Y.Z` with 4 binaries, 4
   `.sha256` files, and one `SHA256SUMS`.
5. Spot-check at least one artifact: download it, verify its checksum against
   the published `.sha256`, run `valyria --version` and confirm it prints
   the tag's version.

To test the pipeline itself without touching real version history, cut a
disposable prerelease first (e.g. bump to `0.0.1-pipeline-test.1`, tag
`v0.0.1-pipeline-test.1`) — see `valyria-app/docs/RELEASING.md`'s rollout
order, which covers both repos' first bootstrap end to end.
