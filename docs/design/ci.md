# CI-only gates

Contract lines: [AGENTS.md → Verification playbook](../../AGENTS.md#verification-playbook).
Workflows: `.github/workflows/{ci,codeql,release,screenshot,pages}.yml`; recipes in `/justfile`.

`just verify` is the local gate and runs every Rust step `ci.yml`'s backend and frontend jobs run,
in the same order. **Keep it that way** — a CI sequence that quietly differs from the documented
one is how the two drift. The one step those jobs add is the frontend's Trunk build
(`just web-build`, the `before*Command` path Tauri uses). The gates below run only on GitHub (or
are measurement-only), so a green local `verify` can still fail CI in exactly these ways.

## Which jobs an event runs (`changes` job)

`ci.yml` always triggers, and its `changes` job decides what each event needs, so required checks
always post (a job skipped via `if` reports success; a `paths-ignore` on the trigger would leave
them pending forever):

- **`code`** — a PR that touches only docs (`docs/`, `*.md`, `LICENSE-*`) skips the Rust and
  WASM jobs; the `Backend (src-tauri)` aggregate turns the skipped matrix into one passing check.
  A push to `main` and a manual dispatch always build. The **weekly cron never builds**: it exists
  for `cargo-audit` and the NinjaOne contract, and the commit it would build already passed.
- **`contract`** — see the NinjaOne contract section below.

`fmt-check` runs on the Linux leg only (formatting is target-independent); clippy and tests run
on all three OSes. Every job carries a `timeout-minutes`, so a hung runner fails in minutes
instead of holding the 6-hour default.

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

## Third-party licenses (`licenses` job)

The outbound half of the license policy: `THIRD-PARTY-LICENSES.md` reproduces the notices of
every crate statically linked into the bundle, from **both** trees (the wasm frontend is
embedded in the binary). `just licenses` runs cargo-about once per crate — they share no
workspace or lockfile — with `about.hbs` (preamble + backend) and `about-web.hbs` (frontend),
against the committed lockfiles, and concatenates the two; this repo's own `publish = false`
crates are ignored so the file does not churn on a version bump. The job re-runs the recipe and
fails on any diff, and first runs `scripts/check-license-lists.sh`, which fails when about.toml's
`accepted` and deny.toml's `allow` lists disagree. A dependency bump therefore needs
`just licenses` committed alongside it. The recipe is `[unix]`: PowerShell redirection would
rewrite encoding and line endings.

## NinjaOne API contract (`ninjaone-contract` job)

Every backend test mocks NinjaOne with hand-written fixtures, so the suite proves what the build
*believes* the API returns. This job fetches the vendor's published OpenAPI spec, re-derives the
digest of the surface we consume (`scripts/ninjaone-spec-digest.py`) and fails when it differs
from `docs/api/ninjaone-surface.md`. A failure is a prompt to read the diff, not necessarily a
break. It depends on a URL this repo does not control, so it runs on the weekly cron and on
manual dispatch, and on a PR or push only when `docs/api/`, `src-tauri/src/api/`, the digest
script or `ci.yml` changed — a vendor outage cannot block unrelated work.

## Repo-tooling gates (`shellcheck`, `actionlint`, `commits` jobs)

- **shellcheck** lints `.claude/hooks/*.sh`, `.githooks/*` and `scripts/*.sh`, then runs
  `.claude/hooks/test.sh` — the hooks' own self-tests, since a hook that parses but decides
  wrongly fails silently.
- **actionlint** checks every workflow (pinned release, verified by checksum; it also runs
  shellcheck over `run:` blocks).
- **commits** applies the `.githooks/commit-msg` Conventional Commits rule to a PR's commits,
  for contributors who never ran `just setup`. PR-only: PRs land as merge commits, so every
  non-merge commit on `main` already passed it.

## CodeQL (GitHub-side)

Rust security queries, build-mode `none` (`.github/workflows/codeql.yml`).

## Manifest versions (GitHub-side)

The `versions` job in `ci.yml` checks that `tauri.conf.json`, `src-tauri/Cargo.toml` and
`web-rs/Cargo.toml` carry the same version on **every PR**. `release.yml`'s guard also compares
them against the tag, but only under `if: startsWith(github.ref, 'refs/tags/')` — i.e. after the
tag and its irreversible release run have been pushed. The two crates share no workspace, so the
version is bumped by hand in three places on every release.

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

## Token and cache exposure in the publishing workflows

Build scripts, proc-macros and npm install scripts run arbitrary third-party code, so a job that
compiles or installs holds only the permissions it must:

- Workflow defaults are `contents: read`; the jobs that publish raise their own (`release.yml`'s
  `create-release`/`build`, `pages.yml`'s `deploy` — the only job with `pages: write` and
  `id-token: write` — and `screenshot.yml`'s `publish` job).
- Checkouts in those workflows use `persist-credentials: false`, so no token sits in
  `.git/config` while dependencies build.
- `screenshot.yml` is two jobs so the write token never shares a runner with third-party code:
  `capture` (read-only, no secrets) builds the demo, runs Playwright and uploads the image as an
  artifact; `publish` checks out main, downloads the image and runs only git and `gh`, with the
  token handed to the push step alone and git hooks disabled there.
- The bundle `build` job in `release.yml` signs with `TAURI_SIGNING_PRIVATE_KEY` in the same
  step that compiles (tauri-action does both), so it restores **no** rust-cache (a cache is
  state other runs wrote) and builds only from the committed lockfiles (`trunk build --locked`
  in `beforeBuildCommand`, a `cargo fetch --locked` preflight). Moving the key into a
  tag-scoped `release` environment is a repo setting — see [RELEASING.md](../RELEASING.md).
- `rust-toolchain.toml` pins the same patch toolchain (`1.98.1`) the workflows install, so the
  components CI adds land on the toolchain the build actually uses.

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
