# Agent Instructions — NinjaOne Patch Toolkit

A **native Rust desktop app for patching-operations teams**. It authenticates to the NinjaOne
Public API with **OAuth 2.0 + PKCE**, filters the fleet, lists per-server patches, computes
compliance / reboot / SLA rollups, and exports to Excel. Tauri 2 backend + Leptos 0.8 (CSR/WASM)
frontend, **edition 2024**, MSRV **1.98** (`rust-toolchain.toml` pins `1.98.1`, the toolchain CI
installs).

Unlike a workspace, the two crates are **independent**: `src-tauri/` (backend, native target) and
`web-rs/` (frontend, `wasm32-unknown-unknown`) each have their own `Cargo.toml` + `Cargo.lock`.
IPC is the global `window.__TAURI__.core.invoke` (`withGlobalTauri`), wrapped in
`web-rs/src/api.rs`.

This file is the **index**, not the contract body. Full per-domain rules live in
`.claude/rules/*.md` — Claude Code auto-loads a rule file when a matching source file is opened,
so every edit sees its rules. **Before planning or designing work in a domain, read its rule
file first** — rules fire on file access, not while you are still thinking. Rationale for every
rule is in [`docs/design/`](./docs/design/README.md). Other agents (Codex, Cursor, Aider): start
from `.claude/rules/` plus the design note for the domain you touch.

## Quick Reference

| Item | Detail |
|---|---|
| **Task runner** | `just` — recipes in `/justfile`; Tauri's `before{Dev,Build}Command` call Trunk directly. |
| **Setup / Dev** | `just setup` once per clone (installs `.githooks`), then `just dev` (`cargo tauri dev`; auto-starts `trunk serve` on `:8080`). |
| **Verify** | `just verify` — the Rust gates CI runs (fmt, clippy, tests, both crates); the justfile is the list. CI adds the Trunk build and the gates in `docs/design/ci.md`. |
| **Crates** | `src-tauri` (backend) + `web-rs` (frontend WASM). No cargo workspace. |
| **NinjaOne spec** | `docs/api/ninjaone-surface.md` is the committed digest of the surface we consume; the weekly `ninjaone-contract` CI job fails when the vendor's spec moves. Verify shapes/params/enums there or in <https://app.ninjarmm.com/apidocs-beta/NinjaRMM-API-v2.yaml> — never infer them. A fixture must emit the vendor's keys, not the ones the code hopes for: build a `DeviceSoftwarePatch` with `model::software_patch_json` (it has **no** `kbNumber`). |

## Domain rules — read the file before you change the domain

| Domain | Rule file (`paths:` in its frontmatter scope it) | Rationale |
|---|---|---|
| Tauri command / IPC chain | `.claude/rules/ipc-contract.md` | `frontend.md` |
| Device actions (write path) | `.claude/rules/actions-write.md` | `actions.md` |
| NinjaOne API client | `.claude/rules/api-client.md` | `api-client.md` |
| Auth | `.claude/rules/auth.md` | `auth.md` |
| Query cache & result store | `.claude/rules/query-cache.md` | `query-cache.md` |
| Backend concurrency & locks | `.claude/rules/backend-core.md` | `concurrency.md` |
| Filter | `.claude/rules/filter.md` | `filter.md` |
| Compliance & rollups | `.claude/rules/compliance.md` | `compliance.md` |
| Severity | `.claude/rules/severity.md` | `severity.md` |
| Frontend (Leptos/WASM) | `.claude/rules/frontend.md` | `frontend.md` |

Cross-cutting flows span several domains — every leg gets its rule file loaded, but read the
whole chain first:

- **New Tauri command** — handler in `commands/<domain>.rs` → register in
  `tauri::generate_handler![]` (`src-tauri/src/lib.rs`) → `ipc!` wrapper + type mirror in
  `web-rs/src/api.rs` + `types.rs`. A mutating handler checks `require_actions_enabled`.
  → `ipc-contract.md`
- **New NinjaOne endpoint** — a method on `NinjaApiClient` using `get_paginated`/`request_raw`;
  never a second reqwest/cursor loop. → `api-client.md`
- **New device action** — POST via `post_action`/`post_json` (`ReplaySafety::ActOnce`) →
  `ActionKind` variant (`is_mutating`/`supports_dry_run`) → dispatch arm in `send_action` →
  button in `ACTION_GROUPS` under its mechanism's heading → mirror in `types.rs`.
  → `actions-write.md`
- **New filter facet** — device facet in `PreparedFilter::device_allowed` (+
  `has_identity_scope`, `patch_filter` if the install `df` honors it); patch facet as a
  client-side `*_allowed()`. → `filter.md`

Layout: the file-by-file repo map lives in [`docs/architecture.md`](./docs/architecture.md) —
read it when orienting; it is deliberately not loaded every session.

## Canonical commands

`just dev` for the daily loop, `just verify` before declaring anything done. Run `just --list`
for the rest; the justfile comments are the documentation. Don't hand-type raw `cargo`
invocations.

The app needs no build-time config: instance, client id and optional secret are entered at
runtime in **Settings** (persisted via the `directories` crate; secrets go to the keyring, never
`settings.json`).

## Coding fundamentals

- No abstraction, configuration, or generality for hypothetical futures (YAGNI).
- Comments explain *why*, not *what*.
- Dependencies are a cost; prefer std lib and existing crate deps.

## Git & version control

- **Conventional Commits required:** `<type>[(scope)][!]: <description>` (enforced by the
  `conventional-commit-validator.sh` PreToolUse hook).
  - Types: `feat fix docs chore refactor test build ci perf style revert deps`
  - Scopes: `desktop`, `web`, `api`, `auth`, `actions`, `export`, `filter`, `settings`,
    `release`, `ci`, `docs`.
- User-facing changes go under `## [Unreleased]` in `CHANGELOG.md`; the release skill rolls it.

## Verification playbook

`just verify` runs CI's Rust gates for both crates; run it before declaring a change done (each
recipe also runs alone — `just --list`). For behavior a unit test can't prove, run `just dev`.
Hook or shell-script changes: `.claude/hooks/test.sh` + `shellcheck`. A dependency bump:
`just licenses` and commit `THIRD-PARTY-LICENSES.md`.

Gates `verify` does not run (Trunk build, coverage, audit/deny, licenses, the NinjaOne contract,
hook tests, actionlint, CodeQL, …) → `docs/design/ci.md`. `cargo-audit` is a required check on
`main`, so a green local `verify` can still fail CI on a new advisory.

## Keeping this file up to date

This index and `.claude/rules/*` are the contract, kept in lockstep: a new rule goes in the
matching rule file (create one if the domain lacks it) and, only if it changes a cross-domain
flow, a line here. When editing these surfaces, update: crate/dir/module changes →
`docs/architecture.md`; toolchain/MSRV/edition → **Quick Reference**; `justfile` recipes →
**Canonical commands**; new command / IPC arg shape / cache / auth / filter / CSP → the domain's
rule file + its table row. Rationale changes → the matching `docs/design/*.md`; the rule line
stays short (one rule, the file, the test, the link). The `agents-md-staleness-check.sh` hook
warns above 30 KB — keep this file near its ~8 KB target; the bytes belong in rule files and
design notes, not in the index.
