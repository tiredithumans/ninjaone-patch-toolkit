# Agent Instructions — NinjaOne Patch Toolkit

A **native Rust desktop app for patching-operations teams**. It authenticates to the NinjaOne
Public API with **OAuth 2.0 + PKCE**, filters the fleet, lists per-server patches, computes
compliance / reboot / SLA rollups, and exports to Excel. Tauri 2 backend + Leptos 0.8 (CSR/WASM)
frontend, **edition 2024**, MSRV **1.96** (`rust-toolchain.toml`).

Unlike a workspace, the two crates are **independent**: `src-tauri/` (backend, native target) and
`web-rs/` (frontend, `wasm32-unknown-unknown`) each have their own `Cargo.toml` + `Cargo.lock`.

This file is the **contract**: one short rule per bullet, the file to read, and the test that
enforces it. The **rationale** behind each rule lives in [`docs/design/`](./docs/design/README.md)
— read the note for a domain before changing it.

## Quick Reference

| Item | Detail |
|---|---|
| **Task runner** | `just` — recipes in `/justfile`; Tauri's `before{Dev,Build}Command` call Trunk directly. |
| **Setup / Dev** | `just dev` (`cargo tauri dev`; auto-starts `trunk serve` on `:8080`). |
| **Verify** | `just verify` — every gate CI runs; the justfile is the list. |
| **Crates** | `src-tauri` (backend) + `web-rs` (frontend WASM). No cargo workspace. |
| **IPC** | Global `window.__TAURI__.core.invoke` (`withGlobalTauri`), wrapped in `web-rs/src/api.rs`. |
| **NinjaOne spec** | Verify endpoint shapes/params/enums against <https://app.ninjarmm.com/apidocs-beta/NinjaRMM-API-v2.yaml> (grep it) — never infer them. |

## Skills

Skills live in `.claude/skills/` and Claude Code loads their descriptions automatically:
**ship**, **feature**, **review**, **release**, **debug**.

## Repo map

```
src-tauri/                       # Tauri 2 backend (native target)
├── src/lib.rs                   # Tauri builder, tracing init, generate_handler![] registry
├── src/main.rs                  # binary entry → lib::run()
├── src/state.rs                 # AppState: auth, api client, settings, tenant-stamped result/fleet caches, job store, confirm-token slot
├── src/state/tests.rs
├── src/auth.rs                  # OAuth2 PKCE (S256, loopback), keyring, single-flight refresh, conditional scope + management grant
├── src/auth/tests.rs
├── src/actions.rs               # device-action domain: ActionKind/JobState/JobReport, pure plan() guardrails, build_parameters
├── src/actions/audit.rs         # append-only action-audit.jsonl (parameters redacted)
├── src/api/                     # NinjaOne Public API client
│   ├── mod.rs                   # NinjaApiClient: /api/v2, bearer, retry policy, cursor paging, single-parse pages
│   ├── devices.rs               # device inventory
│   ├── patches.rs               # current patches + install-history endpoints
│   ├── actions.rs               # WRITE path: patch scan/apply, reboot, script/run, automation-script library
│   ├── activities.rs            # /activities feed used to resolve dispatched jobs
│   └── lookups.rs               # orgs / all-locations / roles / node classes
├── src/filter.rs                # FilterParams → install-query df + PreparedFilter::device_allowed / row facets
├── src/model.rs                 # domain types (Device, Patch, PatchType, PatchStatus, Severity, …)
├── src/rows/                    # join → PatchRow and every rollup off the cached result
│   ├── mod.rs                   # QueryResult / QuerySummary + re-exports of every submodule
│   ├── join.rs                  # device↔patch join, Interner, DeviceLabels, build_rows
│   ├── compliance.rs            # compliance / by-OS / reboot rollups, rollup_device, compliance_scope_note
│   ├── rollups.rs               # failures, severity by org, age buckets, SeverityCounts::BANDS
│   ├── groups.rs                # grouping, sorting, paging over the cache
│   ├── scope.rs                 # QueryScope export provenance
│   ├── table.rs                 # TableCell / TableColumn / format_pct — the shared column definition
│   └── tests.rs
├── src/export.rs                # rust_xlsxwriter workbook (Patches / Compliance / by OS / Needs-Reboot / Failures / About)
├── src/report.rs                # standalone HTML executive report from the cached QueryResult
├── src/settings.rs              # persisted Settings (instance, client id, ports, windows, presets)
├── src/error.rs                 # UiError { message } — the IPC error shape
├── src/commands/                # #[tauri::command] handlers (actions, auth, lookups, patches, export, settings, update)
├── src/commands/patches/tests.rs
├── tauri.conf.json              # CSP, bundle targets, before{Dev,Build}Command, updater (pubkey/endpoint)
├── updater-build.json           # release-only overlay: createUpdaterArtifacts on (signing required)
└── capabilities/default.json    # scoped capability definitions

web-rs/                          # Leptos 0.8 CSR frontend — separate wasm32 crate
├── src/main.rs                  # entry, theme, root mount
├── src/app.rs                   # module decls, shared consts (SEVERITY_OPTIONS), App root + startup wiring
├── src/app/
│   ├── state.rs                 # AppState wrapper + Copy sub-structs by concern; no test module — logic goes to util
│   ├── actions.rs               # ActionBar (the one dispatch surface), ConfirmActionModal, RunAsRoles, JobsTable
│   ├── header.rs · controls.rs · filters.rs · settings.rs · charts.rs · tables.rs · toaster.rs · update.rs
│   ├── modal.rs                 # focus_trap: dialogs take focus on open, keep Tab inside, restore the opener
│   └── util/                    # JS-free pure helpers + their host tests
│       ├── mod.rs · query.rs · selection.rs · filters.rs · pager.rs · format.rs · sort.rs · changelog.rs · tests.rs
├── src/api.rs                   # ipc! macro → typed invoke wrappers + is_tauri() browser-mode guard
├── src/demo.rs                  # pure sample-data builder for demo / web mode
├── src/types.rs                 # request/response types mirrored from the backend
├── styles.css                   # plain global CSS (BEM-ish names); --sev-* band tokens on :root
└── Trunk.toml                   # WASM build/serve (127.0.0.1:8080); never set public_url here

docs/design/                     # rationale behind the rules below, one note per domain
docs/RELEASING.md · docs/TROUBLESHOOTING.md
scripts/                         # screenshot capture tooling (Playwright; not shipped) + changelog-notes.sh
.claude/hooks/                   # commit validator, command parity, AGENTS.md/README staleness, secrets scan; test.sh self-tests them
.github/workflows/               # ci.yml · codeql.yml · pages.yml · release.yml · screenshot.yml
```

## Common patterns

- **New Tauri command** — 3 steps (the `command-parity-check.sh` hook warns if you miss one):
  1. `#[tauri::command] pub fn` (or `async fn` only if it awaits) in `src-tauri/src/commands/<domain>.rs`,
     `State<'_, AppState>` first, `Result<T, UiError>` out.
  2. Add `commands::<domain>::<name>` to `tauri::generate_handler![]` in `src-tauri/src/lib.rs`.
  3. `ipc!(name(arg: T, …) -> Ret)` in `web-rs/src/api.rs` (+ mirror types in `web-rs/src/types.rs`).
     Arg keys and the command string are derived from the wrapper, so they cannot drift.
- **New NinjaOne endpoint** — a method on `NinjaApiClient` (`api/<domain>.rs`) using
  `get_paginated` / `request_raw`; never a second reqwest/cursor loop.
- **New device action** — 4 steps: the POST in `api/actions.rs` via `post_action`/`post_json`
  (`ReplaySafety::ActOnce`); an `ActionKind` variant with correct `is_mutating()` /
  `supports_dry_run()`; the dispatch arm in `commands::actions::send_action`; the button in
  `web-rs/src/app/actions.rs::ACTION_GROUPS` under the heading that names its *mechanism*. Mirror the
  variant in `web-rs/src/types.rs::ActionKind`. → `docs/design/actions.md`
- **New filter facet** — a device facet extends `PreparedFilter::device_allowed` (+
  `has_identity_scope`, and `patch_filter` if the install `df` honors it); a patch facet is a
  client-side `*_allowed()` matched against rows. → `docs/design/filter.md`

## Canonical commands

`just dev` for the daily loop, `just verify` before declaring anything done. Run `just --list` for
the rest; the justfile comments are the documentation. Don't hand-type raw `cargo` invocations.

The app needs no build-time config: instance, client id and optional secret are entered at runtime
in **Settings** (persisted via the `directories` crate; secrets go to the keyring, never
`settings.json`).

## Conventions & gotchas

Backend — commands, cache, concurrency:

- **Tauri commands:** `State<'_, AppState>` first, `Result<T, UiError>` out, registered in
  `generate_handler![]` **and** wrapped by `ipc!`. `async` only when the handler awaits. A mutating
  handler calls `require_actions_enabled` — enforced by
  `every_mutating_command_checks_that_actions_are_enabled`. → `docs/design/frontend.md#tauri-commands`
- **IPC arg keys equal the handler's parameter names, camelCase.** Renaming a parameter is a
  wire-format change; update both sides. → `docs/design/frontend.md#ipc-arg-shape--keys-match-rust-fn-parameter-names-camelcase`
- **`AppState.last_result` is the single source of truth for paging, export and the HTML report.**
  Write via `store_last_result_if_current(token, result)`, read via `with_current_result` /
  `current_result_handle`; never touch the slot directly. → `docs/design/query-cache.md`
- **Claim the `QueryToken` (`begin_query`) before any fetch and redeem it at the store.** A
  superseded or tenant-drifted result is dropped. `StoreOutcome::Superseded` still returns the
  summary; `TenantChanged`/`Poisoned` are errors (`commands::patches::summary_for`). → `docs/design/query-cache.md#the-write-is-generation--and-tenant-gated`
- **Tenant switch, sign-out, sign-in and re-authorize all call `clear_session()`** on the frontend
  and `clear_session_state` on the backend. → `docs/design/query-cache.md#a-tenant-switch-a-sign-out-a-sign-in-and-a-re-authorization-all-clear-the-frontend`
- **Paging/grouping/sorting commands return empty on a cache miss, never an error.** Sorted and
  grouped views are memoized inside `CachedResult`; the cached rows are never reordered. Group
  headers carry no members; never regroup `page_rows` client-side. `demo.rs` mirrors `group_key`. → `docs/design/query-cache.md#paging-commands-return-empty-on-a-miss-never-an-error`
- **Compact aggregates (`failures`, `severity_by_org`, `age_buckets`) ride on both `QueryResult` and
  `QuerySummary`.** Add one in lockstep with `QuerySummary::from_result`, the `types.rs` mirror, the
  demo's `assemble`, and `serialized_shapes_carry_every_frontend_required_key`. `QueryScope` is the
  one `QueryResult`-only exception. → `docs/design/query-cache.md#compact-aggregates-ride-in-the-summary-not-the-rows`
- **Devices and current patches are fetched whole-fleet and scoped client-side** via
  `PreparedFilter::device_allowed`; the OS and third-party families are separate cache slots and
  only the requested family is fetched. Stores are epoch-gated and fetches are single-flight per
  family. `force_refresh` is floored backend-side by `FORCE_MIN_INTERVAL`. → `docs/design/query-cache.md#whole-fleet-prefetch--client-side-scoping`
- **Scoping borrows, never clones.** Rollups take `&[&Patch]`; don't reintroduce an owned
  `Vec<Patch>`. → `docs/design/query-cache.md#scoping-borrows-never-clones`
- **CPU-bound and blocking work goes on `spawn_blocking`** — `assemble_result`, workbook/report
  writes, the audit append, the save dialog, keyring I/O. Judge new code against the rule, not
  against that list. → `docs/design/concurrency.md`
- **`AppState` locks are brief and never held across `.await`.** Take `settings_snapshot()` first;
  hold the result mutex for a handle (`current_result_handle`), not for the work. → `docs/design/concurrency.md`

Auth:

- **Secrets live in the keyring only — never `settings.json`, never a `tracing` event.** The access
  token is in-memory only. → `docs/design/auth.md#secrets-discipline--keyring-only-never-settingsjson-never-logs`
- **PKCE with a loopback redirect on `callback_port`; Native (no secret) and Web (secret) clients
  are both supported.** The callback listener loops over connections. → `docs/design/auth.md`
- **Scope is conditional on `settings.actions.enabled` and the refresh grant never re-sends it.**
  `management_grant()` detects a read-only grant; `None` means unknowable, not denied.
  `reauthorize` drops the keyring refresh token first. → `docs/design/auth.md#scope-is-conditional-and-the-refresh-grant-never-re-sends-it`
- **`store_tokens` assigns in-memory first and downgrades a keyring failure to a warning.**
  `invalidate_access_token(&stale)` no-ops unless the token is still current. → `docs/design/auth.md#in-memory-before-keyring-and-only-the-token-that-got-the-401-is-invalidated`
- **The refresh is single-flight under `refresh_lock`, and only `invalid_grant` clears the
  credential** (`refresh_grant_is_dead`). Not "any 4xx": 429 is retry-later. → `docs/design/auth.md#the-refresh-is-single-flight-and-only-invalid_grant-clears-the-credential`

Write path (device actions) — violating these silently widens the blast radius:

- **Every write POST passes `ReplaySafety::ActOnce`**; a timed-out dispatch becomes
  `JobState::Unknown` and is polled, never replayed. → `docs/design/actions.md#replaysafetyactonce-on-every-post`
- **"Apply all" (native endpoint) and "Apply selected" (library script) are different `ActionKind`s
  under different `ACTION_GROUPS` headings.** Don't collapse them. Remediation script ids resolve
  from Settings, never the request; an unset id or an empty target list is a `plan()` blocker. → `docs/design/actions.md#there-is-no-per-kb-apply-endpoint-so-there-are-two-apply-paths-and-the-ui-names-both`
- **Selection is per patch row; dispatch is per device with per-device targets**
  (`util::targets_by_device` → `ActionRequest.device_targets` → `per_device_parameters`). Ticking a
  row must not tick the device's other rows. No batch-wide `targets` field. → `docs/design/actions.md#selection-is-per-patch-row-dispatch-is-per-device-with-per-device-targets`
- **`build_parameters` encodes by kind:** `kbAllowList=` for OS, `productAllowListB64=` for
  software (NinjaOne splits on spaces). → `docs/design/actions.md#the-parameter-encoding-is-chosen-by-kind`
- **Confirm tokens are payload-bound and single-use.** `request_hash` destructures `ActionRequest`
  exhaustively, hashes the *resolved* script and length-prefixed per-device parameters; `run_action`
  re-plans and re-checks. → `docs/design/actions.md#confirm-tokens-are-payload-bound-and-single-use`
- **Guardrails go in `actions::plan` (`blockers`/`warnings`), not in a dialog.** The `dry_run`
  check is also asserted at the dispatch site. → `docs/design/actions.md#guardrails-live-in-actionsplan`
- **One dispatch surface (`ActionBar`); `Run as` / reboot / `Dry run` are rendered once** and
  labelled with the kinds they reach. → `docs/design/actions.md#there-is-one-dispatch-surface-and-the-run-options-are-shared`
- **After a non-dry-run mutating action call `invalidate_current_patches()`** (and
  `invalidate_fleet_devices()` after a reboot); never `clear_lookups_cache()`; never drop
  `last_result`. A dry run invalidates nothing and raises no stale banner. → `docs/design/actions.md#after-a-mutating-action-invalidate-the-current-patch-cache`
- **Jobs are tenant-stamped; the poller is single-claim** (`try_claim_job_poller` /
  `release_job_poller_if_idle`). Dispatch appends jobs before claiming. → `docs/design/actions.md#job-state-is-tenant-stamped-the-poller-is-single-claim`
- **A job resolves from `/activities` only:** `statusCode` is lifecycle, `activityResult` is the
  verdict, exit code from `data`; `newerThan` is an activity **id**, so the time floor is applied
  client-side; `is_action_activity` lists what the native endpoints emit. → `docs/design/actions.md#resolving-a-dispatched-action-from-activities`

NinjaOne API client:

- **Every call goes through `NinjaApiClient`** (`get_paginated` / `request_raw`); retry is the pure
  `retry_for`; paginated bodies parse once via `parse_page` + `PagedRow`. → `docs/design/api-client.md`
- **Both pagination branches require forward progress; an unreadable cursor is an error, not
  end-of-pages; 5xx/connect retries are `Idempotent`-only.** → `docs/design/api-client.md#both-pagination-branches-require-forward-progress`
- **reqwest has `default-features = false`; keep `gzip`, `http2`, `system-proxy`, `charset`.** → `docs/design/api-client.md#reqwests-default-features-are-off-so-every-one-it-drops-must-be-re-added-explicitly`

Filter:

- **A device facet extends `PreparedFilter::device_allowed`; a patch facet is a client-side
  `*_allowed()`.** `prepare()` once per query; `build_rows` re-checks every row against the scope
  — the install `df` is bandwidth, not the boundary. → `docs/design/filter.md`
- **`organization_ids`/`location_ids`/`role_ids` are multi-select** (empty = all; OR within, AND
  across; `filter::ids` accepts bare or list). `df` grammar: `org=1`, `org in (1, 2)`, token `loc`,
  no `class`. → `docs/design/filter.md#the-three-identity-facets-are-multi-select`

Compliance and rollups — violating these silently misreports a fleet:

- **Every fleet-health rollup uses the `rows::rollup_device` population** (scoped, online,
  `Device::is_patchable`), including the patch loop — pinned by
  `severity_and_age_rollups_cover_the_same_devices_compliance_does`. → `docs/design/compliance.md#one-population-for-every-fleet-health-rollup-via-rowsrollup_device`
- **Every surface prints `rows::compliance_scope_note`** (offline + non-patchable counts;
  `devices_total − devices_offline − devices_unpatchable` is the denominator). The frontend `util`
  mirrors it. → `docs/design/compliance.md#devices_offline-devices_unpatchable-and-patch_families-ride-on-queryresultquerysummary`
- **Both exports print both clocks (`generated_at`, `data_fetched_at`) and the `QueryScope`
  facets in two tiers** (`facets` narrow every sheet; `patch_facets` only the detail rows), built
  from the `QueryPlan`, never the request. Date bounds are absolute UTC via
  `DateTime::from_timestamp`. → `docs/design/compliance.md#both-exports-state-the-facets-from-rowsqueryscope`
- **`Type` is a device-tier chip** — rollups cover only the fetched families. → `docs/design/compliance.md#the-fleet-health-rollups-do-depend-on-the-patch-type-facet`
- **`is_pending` is an exclude list** (not `REJECTED`/`INSTALLED`); current sources get
  `status_override = MANUAL`; `current_status_set` carries every selected status. → `docs/design/compliance.md#rowsis_pending-is-an-exclude-list`
- **`Installed` and `Failed` route to the install-history endpoints; current patches are always
  fetched.** One requested install status is pushed down server-side; the lookback is re-applied
  client-side. → `docs/design/compliance.md#installedfailed-vs-current-patches-status-routing`
- **`format_pct` never rounds up to 100** (caps at 99%; `pct_cell` at one decimal). → `docs/design/compliance.md#a-percentage-never-rounds-up-to-100`
- **There is no patch release date in the API.** `first_seen_at()` is detection time; keep "First
  seen" / "since first seen" naming; fixtures must emit `timestamp`. → `docs/design/compliance.md#there-is-no-patch-release-date-in-the-ninjaone-api`
- **`PatchRow` strings are interned `Arc<str>`** (`rows::Interner`, `DeviceLabels`); the frontend
  mirrors them as `String`. → `docs/design/compliance.md#patchrow-shares-its-repeated-strings-it-does-not-own-them`
- **Tables render through `rows::TableColumn` `COLUMNS`**; the hand-written Leptos headers match
  those spellings by review. → `docs/design/compliance.md#table-headers-come-from-rowstablecolumn-spellings`

Severity:

- **Two vocabularies on one field; `Security`/`Recommended` are their own variants ranked below
  `Important`; unmapped → `Unknown`.** Adding a value touches nine sites — follow the checklist.
  Enumerate bands via `SeverityCounts::BANDS` / `charts::SEV_BANDS`, never a label match —
  `total_severity_is_the_sum_of_its_bands`, `severity_css_defines_every_band`. → `docs/design/severity.md`

Frontend:

- **Server deps never enter `web-rs`**; shared logic is duplicated as plain types. → `docs/design/frontend.md#wasm-gating`
- **CSP governs the webview only; NinjaOne hosts need no `connect-src` change.** The updater is
  backend egress too. → `docs/design/frontend.md#csp-governs-the-webview-not-backend-egress`
- **Non-trivial logic does not belong in a `#[component]` body or in `state.rs`** — put it in the
  `util` module as a free function and test it there. → `docs/design/frontend.md#non-trivial-logic-does-not-belong-in-a-component-body`
- **A dialog calls `modal::focus_trap()` in the closure that creates it**, per instance. → `docs/design/frontend.md#frontend-reactivity-is-closure-based-leptos-csr`
- **`api::is_tauri()` gates every backend touch; `demo.rs` is the only sample-data source and
  demo mode is web-only.** Never set `public_url` in `Trunk.toml`. → `docs/design/frontend.md#demo-mode--browserpages-guard`

## Coding fundamentals

- No abstraction, configuration, or generality for hypothetical futures (YAGNI).
- Comments explain *why*, not *what*.
- Dependencies are a cost; prefer std lib and existing crate deps.

## Git & version control

- **Conventional Commits required:** `<type>[(scope)][!]: <description>` (enforced by the
  `conventional-commit-validator.sh` PreToolUse hook).
  - Types: `feat fix docs chore refactor test build ci perf style revert deps`
  - Scopes: `desktop`, `web`, `api`, `auth`, `export`, `filter`, `settings`, `ci`, `docs`.
- User-facing changes go under `## [Unreleased]` in `CHANGELOG.md`; the release skill rolls it.

## Verification playbook

`just verify` runs every local gate in CI's order; run it before declaring a change done. The
individual recipes (`fmt-check`, `clippy`, `test`, `web-clippy`, `web-test`, …) are callable on
their own — see `just --list`. For behavior a unit test can't prove, run `just dev` and exercise
the view.

CI-only gates (coverage, audit/deny, CodeQL, manifest versions, screenshot tooling, the release
verify job) → `docs/design/ci.md`. `cargo-audit` is a required check on `main`, so a green local
`verify` can still fail CI on a new advisory.

## Keeping this file up to date

When editing these surfaces, update the matching section here:
crate/dir/module changes → **Repo map**; toolchain/MSRV/edition → **Quick Reference**;
`justfile` recipes → **Canonical commands**; new command / IPC arg shape / cache / auth / filter /
CSP → **Common patterns** + **Conventions & gotchas**; CI gate or `tauri.conf.json` bundle →
**Verification playbook** / `docs/design/ci.md`.

Rationale changes → the matching `docs/design/*.md`; the contract line here stays short (one rule,
the file, the test, the link). The `agents-md-staleness-check.sh` hook warns when this file passes
30 KB — that is the budget, and history belongs in the design notes.
