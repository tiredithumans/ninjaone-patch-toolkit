# Agent Instructions — NinjaOne Patch Toolkit

A **native Rust desktop app for patching-operations teams**. It authenticates to the NinjaOne
Public API with **OAuth 2.0 + PKCE**, filters the fleet, lists per-server patches, computes
compliance / reboot / SLA rollups, and exports to Excel. Tauri 2 backend + Leptos 0.8 (CSR/WASM)
frontend, **edition 2024**, MSRV **1.96** (`rust-toolchain.toml`).

Unlike a workspace, the two crates are **independent**: `src-tauri/` (backend, native target) and
`web-rs/` (frontend, `wasm32-unknown-unknown`) each have their own `Cargo.toml` + `Cargo.lock`.

## Quick Reference

| Item | Detail |
|---|---|
| **Task runner** | `just` — recipes in `/justfile`; Tauri's `before{Dev,Build}Command` call Trunk directly. |
| **Setup / Dev** | `just dev` (`cargo tauri dev`; auto-starts `trunk serve` on `:8080`). |
| **Verify** | `just verify` (fmt-check → clippy → test → web-check → web-clippy). |
| **Crates** | `src-tauri` (backend) + `web-rs` (frontend WASM). No cargo workspace. |
| **IPC** | Global `window.__TAURI__.core.invoke` (`withGlobalTauri`), wrapped in `web-rs/src/api.rs`. |

## Skills

| Skill | Trigger text | What it does |
|-------|-------------|--------------|
| **ship** | `"ship"`, `"land this"` | Branch → conventional commits → push → PR → merge → cleanup. |
| **feature** | `"feature X"`, `"add feature X"` | Scaffold a branch, backend command, IPC wrapper, and verify. |
| **review** | `"review"`, `"approve this PR"` | Diff base → head, run verify gates, flag IPC/secret/WASM footguns. |
| **release** | `"release"`, `"bump version"` | Bump the three manifests in lockstep, verify, tag, push (CI builds bundles). |
| **debug** | `"debug X"` | Diagnose Tauri + Leptos WASM issues — walks backend, frontend, auth, IPC. |

Skills live in `.claude/skills/`. Load a skill with `skill: <name>`.

Key files to read before editing:
- **Adding a command?** `src-tauri/src/lib.rs` (handler list) + `web-rs/src/api.rs` (IPC wrappers).
- **NinjaOne API call / pagination?** `src-tauri/src/api/mod.rs` (`NinjaApiClient`, retry + cursor
  paging) → `api/devices.rs`, `api/patches.rs`, `api/lookups.rs`. **Verify endpoint shapes, params,
  and field/status enums against the official spec — never infer them from endpoint names or memory:**
  the rendered docs are <https://app.ninjarmm.com/apidocs/?links.active=core> and the raw OpenAPI is
  <https://app.ninjarmm.com/apidocs-beta/NinjaRMM-API-v2.yaml> (grep it; the SPA can't be scraped).
- **Auth / PKCE / keyring?** `src-tauri/src/auth.rs` + `src-tauri/src/state.rs` (`AppState`).
- **Fleet filter (`df` DSL) / OS-name facet?** `src-tauri/src/filter.rs`.
- **Device↔patch join, compliance, SLA, reboot rollups?** `src-tauri/src/rows.rs`.
- **Excel export?** `src-tauri/src/export.rs` (reads `state.last_result`).
- **Frontend types crossing IPC?** `web-rs/src/types.rs` (mirror of backend arg/result structs).

## Repo map

```
src-tauri/                       # Tauri 2 backend (native target)
├── src/lib.rs                   # Tauri builder, tracing init, generate_handler![] registry
├── src/main.rs                  # binary entry → lib::run()
├── src/state.rs                 # AppState: auth, api client, settings (Mutex), last_result cache
├── src/auth.rs                  # OAuth2 PKCE (S256, loopback redirect), keyring, token refresh
├── src/api/                     # NinjaOne Public API client
│   ├── mod.rs                   # NinjaApiClient: /api/v2, bearer, retry (timeout/429/401), cursor paging
│   ├── devices.rs               # device inventory (df filter)
│   ├── patches.rs               # current patches + install-history endpoints
│   └── lookups.rs               # orgs / locations / roles / node classes
├── src/filter.rs                # FilterParams → df DSL + client-side OS-name / KB-search facets
├── src/model.rs                 # domain types (Device, Patch, PatchType, PatchStatus, …)
├── src/rows.rs                  # join → PatchRow, compliance %, SLA aging, reboot/pending counts
├── src/export.rs                # rust_xlsxwriter workbook (Patches / Compliance / Needs-Reboot)
├── src/settings.rs              # persisted Settings (instance, client id, ports, windows, presets)
├── src/error.rs                 # UiError { message } — the IPC error shape
├── src/commands/                # #[tauri::command] handlers (auth, lookups, patches, export, settings, update)
├── tauri.conf.json              # CSP, bundle targets, before{Dev,Build}Command (Trunk), updater (pubkey/endpoint)
├── updater-build.json           # release-only overlay: createUpdaterArtifacts on (signing required)
├── build.rs                     # tauri-build
└── capabilities/default.json    # scoped capability definitions

web-rs/                          # Leptos 0.8 CSR frontend — separate wasm32 crate
├── src/main.rs                  # entry, theme, root mount
├── src/app.rs                   # views, components, handlers
├── src/api.rs                   # typed invoke(...) wrappers (one per backend command)
├── src/types.rs                 # request/response types mirrored from the backend
├── styles.css                   # plain global CSS (BEM-ish names)
└── Trunk.toml                   # WASM build/serve (127.0.0.1:8080)

.github/workflows/               # ci.yml · codeql.yml · release.yml
```

## Common patterns

- **New Tauri command** — 3 steps (advisory hook `command-parity-check.sh` warns if you miss one):
  1. Implement `#[tauri::command] pub async fn` under `src-tauri/src/commands/<domain>.rs`,
     `State<'_, AppState>` first, `Result<T, UiError>` out.
  2. Add `commands::<domain>::<name>` to `tauri::generate_handler![]` in `src-tauri/src/lib.rs`.
  3. Add a typed `invoke("<name>", …)` wrapper in `web-rs/src/api.rs` (+ mirror types in
     `web-rs/src/types.rs`).

- **New NinjaOne endpoint** — add a method on `NinjaApiClient` (`api/<domain>.rs`); reuse
  `get_paginated` / `request_raw` rather than hand-rolling reqwest + retry + cursor logic.

- **New filter facet** — server-side (identity/class) goes in `FilterParams::device_filter()` (the
  `df` DSL); client-side (substring/text) goes in an `*_allowed()` method matched against rows.

## Canonical commands

All build/dev/verify commands live in `/justfile`. `just` searches upward, so recipes resolve from
any subdirectory.

```bash
just dev             # daily loop — cargo tauri dev (auto-starts trunk serve on :8080)
just web-serve       # frontend-only dev server (trunk serve, :8080)

# CI gates:
just verify          # fmt-check → clippy → test → web-check → web-clippy
just fmt-check       # rustfmt --check BOTH crates (covers web-rs too)
just clippy          # backend clippy (-D warnings)
just web-clippy      # frontend clippy (wasm target, -D warnings)
just test            # backend unit + wiremock integration tests
just web-check       # cargo check the frontend (wasm target)
just web-build       # trunk build → web-rs/dist (debug)

# Dependency policy:
just audit           # RustSec advisories — scans BOTH lockfiles (src-tauri + web-rs)
just deny            # license + supply-chain (sources) + bans policy (deny.toml), backend tree
just web-deny        # same policy for the web-rs tree

# Packaging / housekeeping:
just build           # cargo tauri build → bundles (.dmg/.app, .msi/.nsis, AppImage)
just icon            # regenerate icon formats from src-tauri/icons/icon.png
just clean           # cargo clean both crates + remove web-rs/dist
```

Note: `fmt-check` formats **both** crates, so there is no separate `web-fmt-check`. The frontend has
no test suite, so `verify` runs `web-check` (compile) rather than a `web-test`.

The app needs no build-time config: the **Region/Instance**, **Client ID**, and optional **Secret**
are entered at runtime in **Settings** (persisted to `settings.json` via the `directories` crate;
secrets are **not** stored there — see below).

## Conventions & gotchas

- **Tauri commands:** `#[tauri::command] async fn` → `State<'_, AppState>` first → `Result<T, UiError>`.
  `UiError` serializes to `{ message }`, which the frontend renders in a toast (map errors with
  `.map_err(UiError::from)`). Must be in `generate_handler![]` **and** have an `invoke(...)` wrapper
  in `web-rs/src/api.rs`.

- **IPC arg shape — keys match Rust fn parameter names (camelCase).** The frontend wrapper builds an
  arg object whose keys equal the handler's parameter names. A handler taking `args: PatchQueryArgs`
  is invoked with `{ args: {...} }`; one taking `org_id: i64` is invoked with `{ orgId: ... }`. Arg
  structs use `#[serde(rename_all = "camelCase")]`. Renaming a parameter is a wire-format change —
  update both sides.

- **`query_patches` → cache → export/paging coupling (load-bearing).** `query_patches` caches the
  full `QueryResult` in `AppState.last_result` (a `Mutex`) on success and returns only a lightweight
  `QuerySummary` (first page of rows + `rows_total` + the reboot-device subset + compliance) over IPC
  — a 10k+ row fleet is never serialized wholesale into the WASM webview. The detail table pages the
  rest on demand via `get_patch_rows(offset, limit)`, which slices the same cache; `export_patches_xlsx`
  reads it too. So the cache is the single source of truth for both export **and** row paging: export
  or paging with no prior successful query = empty. Don't add a second source of truth for the rows,
  and keep the backend `QuerySummary` ⇄ frontend `QueryResult` (`web-rs/src/types.rs`) shapes in sync.

- **`AppState` locks are brief — never held across `.await`.** `settings`/`last_result` are
  `std::sync::Mutex`. Take a `settings_snapshot()` (clone) before any `.await`; don't hold a guard
  across an API call.

- **Secrets discipline — keyring only, never `settings.json`, never logs.** The refresh token and
  optional client secret live in the OS keyring (Keychain / Credential Manager / Secret Service).
  The access token is in-memory only. `settings.json` holds non-sensitive config (instance URL,
  client id, ports, windows, presets). Never write a token/secret to disk or a `tracing` event.

- **Auth: PKCE, lazy token, Native-or-Web client.** `AuthState::access_token()` refreshes lazily
  before each call. Sign-in is the interactive S256 PKCE flow with a **loopback** redirect on the
  configured `callback_port` (default `11434`); a hung sign-in usually means the callback never
  arrived. Scope is read-only `monitoring offline_access`. **Native** (public) clients have **no**
  secret; **Web** (confidential) clients do — the app supports both, so don't hardcode either.

- **NinjaOne API client — reuse the shared retry + pagination.** Every call goes through
  `NinjaApiClient` (`api/mod.rs`): `{base}/api/v2{path}`, bearer auth, retry on timeout / 429
  (honors `Retry-After`) / 401 (forces a token refresh). `get_paginated` handles **both** a bare
  JSON array **and** the `{ results, cursor }` envelope, where `cursor` may be a string or a
  `{ name, offset, … }` object; it stops when a page returns 0 rows even if the server echoes a
  stale token. Don't hand-roll a second reqwest/cursor loop.

- **Filter — server-side `df` DSL vs client-side facets.** `FilterParams::device_filter()` builds
  the NinjaOne `df` string from identity facets (`org`/`location`/`role`) + the coarse OS-type facet
  (`class in (...)`, upper-cased), returning `None` to mean "whole fleet". The granular OS-name
  substring (`os_name_allowed`) and free-text KB/name search (`search_allowed`, which accepts a `KB`
  prefix on either side) are applied **client-side** against rows after fetch. Keep the split: a new
  identity/class facet extends the DSL; a new substring/text facet is a client-side `*_allowed()`.

- **Installed/Failed vs current patches (status routing — load-bearing).** Per the official spec,
  the current `/queries/{os,software}-patches` feed returns only patches "for which there were **no
  installation attempts**" (statuses `MANUAL`/`APPROVED`/`REJECTED`), while `/queries/*-patch-installs`
  returns the install **history** — "successful **and** failed" records (status `INSTALLED`/`FAILED`).
  So **both** `Installed` *and* `Failed` are install *results* and must route to the install-history
  endpoints over the lookback window (`settings.install_window_days`, overridable per query); only
  `Pending`/`Approved`/`Rejected` narrow the current feed. `PatchStatus::is_install_history()` encodes
  this. Routing `Failed` to the current feed (it never appears there) was a real bug — a FAILED query
  returned nothing. Current patches are **always** fetched regardless of the status filter (they drive
  compliance % and pending/reboot counts). See `commands/patches.rs`.

- **camelCase ↔ snake_case across IPC.** Backend arg/result structs sent to/from the frontend carry
  `#[serde(rename_all = "camelCase")]`; `web-rs/src/types.rs` mirrors them. NinjaOne API JSON (e.g.
  `systemName`, `nodeClass`) is deserialized inside the backend models — that's separate from the
  IPC wire format.

- **WASM gating.** `web-rs` compiles to `wasm32-unknown-unknown` and is a **separate crate**. Server
  deps (tokio, reqwest, keyring, rust_xlsxwriter) belong in `src-tauri` only — never pull them into
  `web-rs`. Shared logic that must run in both is duplicated as plain types, not shared via a crate.

- **CSP governs the webview, not backend egress.** `connect-src` in `tauri.conf.json` is
  `'self' ipc: http://ipc.localhost` — the webview only talks to the backend over IPC. **All**
  NinjaOne HTTP happens in the Rust backend (reqwest), so adding a new NinjaOne region/host needs
  **no** CSP change. Don't add `connect-src` entries for backend calls.

- **Auto-update.** `commands::update::{check_for_update, install_update}` wrap `tauri-plugin-updater`;
  the frontend's `UpdateSplash` shows the release notes (changelog) and the install relaunches the
  app. The updater fetches the signed `latest.json` from the GitHub releases endpoint
  (`tauri.conf.json` → `plugins.updater`) — **backend egress, not subject to the CSP**. The launch
  check is gated by the `auto_check_updates` setting. `createUpdaterArtifacts` is **off** in the base
  config (so local `just build` needs no signing key) and enabled only in the release via
  `--config src-tauri/updater-build.json`. The minisign **public** key is committed in
  `tauri.conf.json`; the **private** key + password are GitHub secrets
  (`TAURI_SIGNING_PRIVATE_KEY[_PASSWORD]`). Updates apply only from a build that already contains the
  updater, and only once a release is **published** (a draft isn't `latest`). The notes shown in
  `UpdateSplash` come from `CHANGELOG.md`: `release.yml` extracts the tagged version's section and
  passes it to tauri-action as `releaseBody`, which becomes both the GitHub release body and
  `latest.json`'s `notes`. Add user-facing changes under `## [Unreleased]` in `CHANGELOG.md`; the
  release skill rolls it to the version heading at tag time.

- **Frontend reactivity is closure-based (Leptos CSR).** `{move || sig.get()}` to track, `.get()` /
  `.with()` to read; state is `RwSignal<T>`. CSS is plain global `web-rs/styles.css`.

## Coding fundamentals

- Match the style, structure, and idioms of the file you're editing.
- Solve the task at hand; don't refactor unrelated code or expand scope.
- No abstraction, configuration, or generality for hypothetical futures (YAGNI).
- Comments explain *why*, not *what*.
- Dependencies are a cost; prefer std lib and existing crate deps.
- Security first: no secrets to disk or logs; tokens stay in keyring/memory.
- Test what you change; keep the suite green.

## Git & version control

- **Conventional Commits required:** `<type>[(scope)][!]: <description>` (enforced by the
  `conventional-commit-validator.sh` PreToolUse hook).
  - Types: `feat fix docs chore refactor test build ci perf style revert deps`
  - Scopes: `desktop`, `web`, `api`, `auth`, `export`, `filter`, `settings`, `ci`, `docs`.

## Verification playbook

Run the same gates CI runs before declaring a change done. `just verify` is the single command;
each gate is also callable independently. Use the recipe flags from `/justfile`; don't hand-type raw
`cargo` invocations.

1. **Format** — `just fmt-check` (both crates).
2. **Lint (backend)** — `just clippy` (`-D warnings`).
3. **Test** — `just test` (backend unit + wiremock integration).
4. **Frontend compile** — `just web-check` (wasm target; `web-rs` is a separate crate the backend
   gates never reach).
5. **Lint (frontend)** — `just web-clippy` (`-D warnings`, wasm target).
6. **Dependency audit** *(optional locally)* — `just audit` (RustSec advisories, both lockfiles)
   + `just deny` / `just web-deny` (licenses + supply-chain sources + bans via `deny.toml`).
7. **CodeQL** *(GitHub-side)* — Rust security queries, build-mode `none` (`.github/workflows/codeql.yml`).

For behavior changes not provable by a unit test, run `just dev` and exercise the view.

## Keeping this file up to date

When editing these surfaces, update the matching section here:
crate/dir/module changes → **Repo map**; toolchain/MSRV/edition → **Quick Reference**;
`justfile` recipes → **Canonical commands** + **Verification playbook**;
new command / IPC arg shape / cache / auth / filter / CSP → **Common patterns** + **Conventions & gotchas**;
CI gate or `tauri.conf.json` bundle → **Verification playbook**.

The staleness hook (`agents-md-staleness-check.sh`) reminds you if you forget.
