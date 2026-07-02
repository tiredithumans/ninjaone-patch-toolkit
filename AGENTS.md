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
| **Verify** | `just verify` (fmt-check → clippy → test → web-check → web-clippy → web-test). |
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
├── src/state.rs                 # AppState: auth, api client, settings (Mutex), last_result + whole-fleet device/current-patch caches
├── src/auth.rs                  # OAuth2 PKCE (S256, loopback redirect), keyring, token refresh
├── src/api/                     # NinjaOne Public API client
│   ├── mod.rs                   # NinjaApiClient: /api/v2, bearer, retry (timeout/429/401), cursor paging
│   ├── devices.rs               # device inventory (df filter)
│   ├── patches.rs               # current patches + install-history endpoints
│   └── lookups.rs               # orgs / locations / roles / node classes
├── src/filter.rs                # FilterParams → install-query df DSL + client-side device_allowed (identity scope) / OS-name / KB-search facets
├── src/model.rs                 # domain types (Device, Patch, PatchType, PatchStatus, …)
├── src/rows.rs                  # join → PatchRow, compliance %, SLA aging, reboot/pending + failure/severity/age rollups
├── src/export.rs                # rust_xlsxwriter workbook (Patches / Compliance / Needs-Reboot / Patch Failures)
├── src/report.rs                # standalone HTML executive report (inline SVG charts) from the cached QueryResult
├── src/settings.rs              # persisted Settings (instance, client id, ports, windows, presets)
├── src/error.rs                 # UiError { message } — the IPC error shape
├── src/commands/                # #[tauri::command] handlers (auth, lookups, patches, export, settings, update)
├── tauri.conf.json              # CSP, bundle targets, before{Dev,Build}Command (Trunk), updater (pubkey/endpoint)
├── updater-build.json           # release-only overlay: createUpdaterArtifacts on (signing required)
├── build.rs                     # tauri-build
└── capabilities/default.json    # scoped capability definitions

web-rs/                          # Leptos 0.8 CSR frontend — separate wasm32 crate
├── src/main.rs                  # entry, theme, root mount
├── src/app.rs                   # module decls, shared consts, App root + startup wiring
├── src/app/                     # state + view components as descendant modules of `app`
│   ├── state.rs                 # AppState wrapper (single context) + 8 Copy sub-structs grouped by concern (session/lookups/filters/query/run/settings/updates/ui) + Tab/AppliedFilters/Toast/Progress
│   ├── header.rs                # Header (sign-in/out, settings toggle)
│   ├── controls.rs              # RunControls + PresetRow (run/refresh cadence, exports, presets)
│   ├── filters.rs               # Filters panel
│   ├── settings.rs              # SettingsPanel
│   ├── charts.rs                # Compliance-tab inline-SVG charts (compliance / severity / age) + host-tested geometry
│   ├── tables.rs                # Results tabs: Patches / Compliance (charts + table) / Needs Reboot / Failures
│   ├── toaster.rs               # Toaster (aria-live toast region)
│   ├── update.rs                # UpdateSplash modal + changelog-notes rendering
│   └── util.rs                  # JS-free pure helpers (format/parse/CSS-class/sort) + their host tests
├── src/api.rs                   # typed invoke(...) wrappers + is_tauri() browser-mode guard
├── src/demo.rs                  # pure sample-data builder (QueryResult) for demo / web mode
├── src/types.rs                 # request/response types mirrored from the backend
├── styles.css                   # plain global CSS (BEM-ish names)
└── Trunk.toml                   # WASM build/serve (127.0.0.1:8080)

scripts/                         # dev/CI tooling (not shipped)
└── screenshot.mjs               # headless-Chromium capture of the web demo → docs/images/screenshot.png (Playwright)

.github/workflows/               # ci.yml · codeql.yml · pages.yml · release.yml · screenshot.yml
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

- **New filter facet** — an identity/scope facet (matched against a cached device) extends
  `FilterParams::device_allowed` (+ `has_identity_scope`) and, if the install-history `df` honors it,
  `patch_filter`; a substring/text facet goes in an `*_allowed()` method matched against rows.

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
just coverage        # backend test coverage (cargo-llvm-cov) → summary + target/lcov.info
just web-check       # cargo check the frontend (wasm target)
just web-test        # frontend pure-helper unit tests (host target; wasm excludes them)
just web-build       # trunk build → web-rs/dist (debug)
just web-build-pages # release build with the Pages subpath base href (used by pages.yml)

# Dependency policy:
just audit           # RustSec advisories — scans BOTH lockfiles (src-tauri + web-rs); accepted advisories live in .cargo/audit.toml (justification + revisit note required)
just deny            # license + supply-chain (sources) + bans policy (deny.toml), backend tree
just web-deny        # same policy for the web-rs tree

# Packaging / housekeeping:
just build           # cargo tauri build → bundles (.dmg/.app, .msi/.nsis, AppImage)
just icon            # regenerate icon formats from src-tauri/icons/icon.png
just screenshot      # rebuild the README demo screenshot via headless Chromium (Playwright; also a CI workflow)
just clean           # cargo clean both crates + remove web-rs/dist
```

Note: `fmt-check` formats **both** crates, so there is no separate `web-fmt-check`. The frontend's
`web-test` covers only the JS-free **pure helpers** (run on the host target; the wasm build excludes
the `#[cfg(test)]` module). Components and `js_sys`-backed helpers aren't unit-tested, so `verify`
still leans on `web-check` (compile) + `web-clippy` for the rest of the frontend.

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
  `QuerySummary` (first page of rows + `rows_total` + the reboot-device subset + compliance + the
  compact dashboard/failure aggregates) over IPC — a 10k+ row fleet is never serialized wholesale into
  the WASM webview. The detail table pages the rest on demand via `get_patch_rows(offset, limit)`,
  which slices the same cache; `export_patches_xlsx` **and** `export_report_html` read it too. So the
  cache is the single source of truth for export, the HTML report, **and** row paging: any of them with
  no prior successful query = empty. Don't add a second source of truth for the rows.
  `get_patch_rows` also takes an optional `sort` (`rows::RowSort`) and re-orders **per request** via a
  ref-sort in `rows::page_rows` — the cached rows themselves are never reordered; their canonical
  severity/org/device order feeds the export and the summary's inline first page.
  - **Compact aggregates ride in the summary, not the rows.** Fleet-wide distributions the frontend
    charts/failure tab need — `failures` (FAILED-install rollup, `build_failures`), `severity_by_org`
    (`build_severity_by_org`), `age_buckets` (`build_age_buckets`) — are computed backend-side in
    `rows.rs` and carried on **both** `QueryResult` (cached; the HTML report reads it) and `QuerySummary`
    (IPC; the dashboard reads it). They're bounded (one entry per failing patch / per org / 5 buckets),
    so they ship whole rather than paged. Add such a field in lockstep: `QueryResult` + `QuerySummary` +
    clone in `QuerySummary::from_result` + the `web-rs/src/types.rs` mirror + the demo's `assemble`, and
    assert its key in `serialized_shapes_carry_every_frontend_required_key`. Keep the backend
    `QuerySummary` ⇄ frontend `QueryResult` (`web-rs/src/types.rs`) shapes in sync.

- **Whole-fleet prefetch + client-side scoping (load-bearing).** The device inventory and current
  patches (OS + 3rd-party) are fetched **whole-fleet** (no `df`) and cached in `AppState`
  (`fleet_devices_cache`, `DEVICE_TTL` ~15 min since devices change rarely; `fleet_current_cache`,
  refreshed on `force_refresh` or past `CURRENT_PATCHES_TTL`). `run_query` then scopes them to the
  selected identity facets (org/location/role/class) **client-side** via `FilterParams::device_allowed`
  — so changing org/location/role/type/severity re-filters the cache with **no** round trip. This is
  why `query_patches` takes the cached devices/current as *futures* (concurrent cold fetch) and why
  `device_filter` no longer exists. **`force_refresh`** (camelCase `forceRefresh`, the auto-refresh tick
  / manual ↻) bypasses the current-patch TTL to pull fresh patch state mid-patching; a normal Run query
  leaves it false. Install history is **not** prefetched — it's fetched fresh per query, scoped
  server-side by `patch_filter` + status-pushed-down (too large to cache). The summary carries
  `data_fetched_at` (when the patch data was last fetched, distinct from `generated_at`) for the UI's
  "patch data as of …" label. The whole-fleet caches are tenant-scoped, so `clear_lookups_cache` drops
  them too. Trade-off: the scoped current-patch subset is cloned out of the `Arc` cache per query
  (bounded by scope; a one-off larger clone only in the whole-fleet view) so the rollups keep consuming
  owned `&[Patch]` slices unchanged.

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

- **Filter — client-side identity scope vs server-side install `df` vs client-side facets.** Because
  devices/current patches are prefetched whole-fleet (above), **all** identity facets
  (`org`/`location`/`role` + the coarse OS-type `class`) are matched **client-side** by
  `FilterParams::device_allowed` (case-insensitive class), and `has_identity_scope` reports whether any
  is active. The install-history queries, fetched fresh per query, still send identity facets
  server-side via `FilterParams::patch_filter` (the `df`; `class` is omitted — `/queries/*` ignore it —
  and reapplied via the device join in `build_rows`). The granular OS-name substring (`os_name_allowed`)
  and free-text KB/name search (`search_allowed`, which accepts a `KB` prefix on either side) are
  applied **client-side** against rows after fetch. Keep the split: an identity/scope facet extends
  `device_allowed`; a substring/text facet is a client-side `*_allowed()`.

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
  - **Install-status pushdown.** The `*-patch-installs` endpoints honor a server-side `status`
    (`FAILED`/`INSTALLED`). When the operator requests **exactly one** install status, `run_query`
    passes it to `fleet_*_patch_installs` so a FAILED-only (failure-dashboard) query doesn't download
    the window's successful installs just to drop them; with **both** requested it's left unset (both
    records are needed). The client-side `install_status_set` narrowing in `build_rows` stays as a
    backstop. The current feed is **not** status-filtered server-side — narrowing it would starve the
    compliance/severity/age rollups, which need the full `MANUAL`/`APPROVED`/`REJECTED` set.

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

- **Demo mode + browser/Pages guard.** The same frontend serves two contexts. Inside Tauri it talks
  to the backend over IPC; in a plain browser (the GitHub Pages live demo) there is **no** backend.
  `api::is_tauri()` (checks `window.__TAURI__`) gates this: `invoke` and `on_query_progress` no-op
  outside Tauri so an undefined global never throws, and `App` startup branches — under Tauri it runs
  the auth/lookups/settings flow; in a browser it sets `web_mode` and calls `enter_demo()`.
  `web-rs/src/demo.rs` is the **only** source of sample data — pure builders (no `js_sys`/IPC), so
  they host-test via `just web-test`. `enter_demo()` seeds the org/role/OS-type lookup dropdowns from
  the sample and flags `demo`, but leaves the results **empty** ("Run a query to list patches") until
  the user presses **Run query** — exactly like the real app. **Run query** routes to `run_demo_query`
  → `demo::filtered_result(...)`, which mirrors the backend's *display* filtering (identity/class/text
  facets + date windows) over the sample rows so the demo's controls actually filter — Compliance/
  Reboot stay representative (narrowed only by org). Demo mode is **web-only**: there is no
  "load sample data" affordance and the desktop release never enters it (no auto-load → `demo` stays
  false and the normal auth path runs). `web_mode` also disables the backend-only actions (sign-in,
  **export**). The Pages build (`just web-build-pages`,
  `.github/workflows/pages.yml`) sets the subpath base href via `--public-url` — **never** put
  `public_url` in `Trunk.toml`, or Tauri's relative-dist webview breaks. Pages deploys only from
  `main`; backend features (queries, export, auth) are desktop-only and intentionally inert in the
  hosted demo.

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
   **Test (frontend)** — `just web-test` (pure helpers, host target; wasm excludes the test module).
6. **Coverage** *(measurement-only; CI `coverage` job)* — `just coverage` (cargo-llvm-cov, backend
   only). No minimum threshold is enforced yet, so a dip never fails the build; the CI job publishes
   `lcov.info` as an artifact and a per-file summary on the run page.
7. **Dependency audit** *(optional locally)* — `just audit` (RustSec advisories, both lockfiles)
   + `just deny` / `just web-deny` (licenses + supply-chain sources + bans via `deny.toml`).
8. **CodeQL** *(GitHub-side)* — Rust security queries, build-mode `none` (`.github/workflows/codeql.yml`).

For behavior changes not provable by a unit test, run `just dev` and exercise the view.

## Keeping this file up to date

When editing these surfaces, update the matching section here:
crate/dir/module changes → **Repo map**; toolchain/MSRV/edition → **Quick Reference**;
`justfile` recipes → **Canonical commands** + **Verification playbook**;
new command / IPC arg shape / cache / auth / filter / CSP → **Common patterns** + **Conventions & gotchas**;
CI gate or `tauri.conf.json` bundle → **Verification playbook**.

The staleness hook (`agents-md-staleness-check.sh`) reminds you if you forget.
