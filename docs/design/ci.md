# CI-only gates

Contract lines: [AGENTS.md → Verification playbook](../../AGENTS.md#verification-playbook).
Workflows: `.github/workflows/{ci,codeql,release,screenshot,pages}.yml`; recipes in `/justfile`.

`just verify` is the local gate and runs every step `ci.yml`'s Rust and frontend jobs run, in the
same order. **Keep it that way** — a CI sequence that quietly differs from the documented one is
how the two drift. The gates below run only on GitHub (or are measurement-only), so a green local
`verify` can still fail CI in exactly these ways.

## Coverage (measurement-only; `coverage` job)

`just coverage` (cargo-llvm-cov, backend only). No minimum threshold is enforced yet, so a dip
never fails the build; the CI job publishes `lcov.info` as an artifact and a per-file summary on
the run page.

## Dependency audit (CI-enforced; optional locally)

`just audit` (RustSec advisories, both lockfiles) + `just deny` / `just web-deny` (licenses +
supply-chain sources + bans via `deny.toml`). `ci.yml` runs these as the dedicated `audit` and
`deny` jobs, and `cargo-audit` is a **required check** on `main` — so these are gates, not advice.
`just verify` deliberately does **not** chain them (they hit the network and the advisory DB
moves under you), which is the one way a green local `verify` can still fail CI. Accepted
advisories live in `.cargo/audit.toml` (justification + revisit note required).

## CodeQL (GitHub-side)

Rust security queries, build-mode `none` (`.github/workflows/codeql.yml`).

## Manifest versions (GitHub-side)

The `versions` job in `ci.yml` checks that `tauri.conf.json`, `src-tauri/Cargo.toml` and
`web-rs/Cargo.toml` carry the same version on **every PR**. `release.yml`'s guard also compares
them against the tag, but only under `if: startsWith(github.ref, 'refs/tags/')` — i.e. after the
tag and its irreversible release run have been pushed. The two crates share no workspace, so this
is bumped by hand and the manifests co-change in ~23 of every 300 commits.

## Screenshot tooling (release-only)

`just screenshot-test` runs `scripts/*.test.mjs` (node:test) over the capture tool's
TLS/static-server path: browser-free, no built dist, seconds. It runs in **`release.yml`'s
`verify` job only** — not in `ci.yml`, not in `just verify` (which is the Rust gate and must not
start requiring Node). Placed there because `create-release` `needs:` that job, so a tool broken
by a dependency bump refuses the release rather than surfacing afterwards: `screenshot.yml` fires
on `release: published`, i.e. once the release already exists and a failure only means the README
image silently fails to refresh. That is the exact hole `selfsigned` 2 → 5 fell through —
`generate` became async and the un-awaited call handed `undefined` key/cert to the HTTPS server.
The trade-off is deliberate: a break now lands on `main` green and is caught at tag time instead
of in review.

## Release gate (GitHub-side)

`release.yml`'s `verify` job runs `just verify` on the tagged commit and `create-release` `needs:`
it, so a release cannot be cut from a commit that fails the gates. This is not redundant with
`ci.yml`: a tag can point at any commit — one that never went through a PR, or a `main` that went
red since its last green run — and `release.yml` also accepts `workflow_dispatch` on an arbitrary
ref. Without it, signed bundles that the **auto-updater distributes to every install** could be
built from an unverified commit, which is the least reversible thing in this repo. One OS, not the
matrix: the per-OS legs already ran at PR time.

## Auto-update packaging

`createUpdaterArtifacts` is **off** in the base config (so local `just build` needs no signing
key) and enabled only in the release via `--config src-tauri/updater-build.json`. The minisign
**public** key is committed in `tauri.conf.json`; the **private** key + password are GitHub
secrets (`TAURI_SIGNING_PRIVATE_KEY[_PASSWORD]`). Updates apply only from a build that already
contains the updater, and only once a release is **published** (a draft isn't `latest`). The notes
shown in `UpdateSplash` come from `CHANGELOG.md`: `release.yml` extracts the tagged version's
section and passes it to tauri-action as `releaseBody`, which becomes both the GitHub release
body and `latest.json`'s `notes`. Add user-facing changes under `## [Unreleased]` in
`CHANGELOG.md`; the release skill rolls it to the version heading at tag time. Key handling and
rotation: [RELEASING.md](../RELEASING.md).
