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
- **Auth / PKCE / keyring / scope?** `src-tauri/src/auth.rs` + `src-tauri/src/state.rs` (`AppState`).
- **Device action (apply / reboot / run script)?** `src-tauri/src/actions.rs` (guardrails +
  job model) → `src-tauri/src/api/actions.rs` (the POSTs) → `src-tauri/src/commands/actions.rs`
  (plan/confirm/dispatch/poll) → `web-rs/src/app/actions.rs` (UI).
- **Fleet filter (`df` DSL) / OS-name facet?** `src-tauri/src/filter.rs`.
- **Device↔patch join, compliance, SLA, reboot rollups?** `src-tauri/src/rows.rs`.
- **Excel export?** `src-tauri/src/export.rs` (reads `state.last_result`).
- **Frontend types crossing IPC?** `web-rs/src/types.rs` (mirror of backend arg/result structs).

## Repo map

```
src-tauri/                       # Tauri 2 backend (native target)
├── src/lib.rs                   # Tauri builder, tracing init, generate_handler![] registry
├── src/main.rs                  # binary entry → lib::run()
├── src/state.rs                 # AppState: auth, api client, settings (Mutex), last_result + whole-fleet device + per-family current-patch caches + tenant-stamped job store & confirm-token slot
├── src/auth.rs                  # OAuth2 PKCE (S256, loopback redirect), keyring, token refresh, conditional scope + management-grant detection
├── src/actions.rs               # device-action domain: ActionKind/JobState/JobReport, pure `plan()` guardrails, build_parameters, activity correlation
├── src/actions/audit.rs         # append-only action-audit.jsonl (parameters redacted)
├── src/api/                     # NinjaOne Public API client
│   ├── mod.rs                   # NinjaApiClient: /api/v2, bearer, retry (timeout/connect/5xx/429/401 + ReplaySafety), cursor paging
│   ├── devices.rs               # device inventory (df filter)
│   ├── patches.rs               # current patches + install-history endpoints
│   ├── actions.rs               # WRITE path: patch scan/apply, reboot, script/run, automation-script library
│   ├── activities.rs            # /activities feed used to resolve dispatched jobs
│   └── lookups.rs               # orgs / locations / roles / node classes
├── src/filter.rs                # FilterParams → install-query df DSL + client-side device_allowed (identity scope) / OS-name / KB-search facets
├── src/model.rs                 # domain types (Device, Patch, PatchType, PatchStatus, …)
├── src/rows.rs                  # join → PatchRow, compliance %, SLA aging, reboot/pending + failure/severity/age rollups
├── src/export.rs                # rust_xlsxwriter workbook (Patches / Compliance / Needs-Reboot / Patch Failures)
├── src/report.rs                # standalone HTML executive report (inline SVG charts) from the cached QueryResult
├── src/settings.rs              # persisted Settings (instance, client id, ports, windows, presets)
├── src/error.rs                 # UiError { message } — the IPC error shape
├── src/commands/                # #[tauri::command] handlers (actions, auth, lookups, patches, export, settings, update)
├── tauri.conf.json              # CSP, bundle targets, before{Dev,Build}Command (Trunk), updater (pubkey/endpoint)
├── updater-build.json           # release-only overlay: createUpdaterArtifacts on (signing required)
├── build.rs                     # tauri-build
└── capabilities/default.json    # scoped capability definitions

web-rs/                          # Leptos 0.8 CSR frontend — separate wasm32 crate
├── src/main.rs                  # entry, theme, root mount
├── src/app.rs                   # module decls, shared consts, App root + startup wiring
├── src/app/                     # state + view components as descendant modules of `app`
│   ├── state.rs                 # AppState wrapper (single context) + 9 Copy sub-structs grouped by concern (session/lookups/filters/query/run/settings/updates/ui/actions) + Tab/AppliedFilters/Toast/Progress/DeviceSelection
│   ├── actions.rs               # ActionBar (the one dispatch surface: selection + ACTION_GROUPS + shared run options + folded-in ScriptPicker), ConfirmActionModal, RunAsRoles, JobsTable (history only)
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
  3. Add a typed wrapper in `web-rs/src/api.rs` via the `ipc!` macro (+ mirror types in
     `web-rs/src/types.rs`). `ipc!(name(arg: T, …) -> Ret)` generates the camelCase arg struct and
     the `invoke` call, so the arg keys equal the wrapper's parameter names and the command string
     equals the wrapper's name **by construction** — both of which the hand-written version left
     free to drift. A wrapper deliberately named differently spells the target out:
     `ipc!(export_patches as "export_patches_xlsx", () -> Option<String>)`.

- **New NinjaOne endpoint** — add a method on `NinjaApiClient` (`api/<domain>.rs`); reuse
  `get_paginated` / `request_raw` rather than hand-rolling reqwest + retry + cursor logic.

- **New device action** — 4 steps, and the middle two are what keep it safe:
  1. Add the POST to `api/actions.rs` via `post_action` / `post_json` (both pass
     `ReplaySafety::ActOnce` — never call `request_raw` with `Idempotent` for a write).
  2. Add a variant to `actions::ActionKind` and make `is_mutating()` / `supports_dry_run()`
     answer correctly — those two decide whether the confirm gate, the blast-radius cap, the
     org-span cap and the maintenance window apply.
  3. Dispatch it from the `match kind` in `commands::actions::send_action`.
  4. Add the button to `web-rs/src/app/actions.rs::ACTION_GROUPS`, under the heading that names
     its *mechanism* — the group label is what tells the operator how wide the blast radius is.
  Mirror the variant in `web-rs/src/types.rs::ActionKind` (labels, `is_mutating`, `can_reboot`,
  `is_remediation`, `runs_a_script`, `is_os_family`) — the frontend has its own copy.

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

**Non-trivial logic therefore does not belong in a `#[component]` body** — put it in
`web-rs/src/app/util.rs` as a free function and test it there. The same rule covers `state.rs`,
which is not a component file: `filter_params` (the `FilterParams` mapping behind *every* query,
lifted out of `FilterState::current_filter`), `parse_clamped` / `parse_optional_id` (the settings
number fields — `<input type="number">` treats `min`/`max` as advisory, so the clamp is the real
guard), and `action_disabled_reason` / `selection_summary` all live in `util.rs` for this reason.
`date_to_epoch` / `epoch_to_date` are plain civil-date arithmetic rather than `js_sys::Date`, so
they host-test too — and `demo.rs` shares them instead of keeping the second copy it used to. A `#[component]` can only be
compile-checked, so arithmetic written inline inside one is unreachable by any test. The pager
(`page_count`/`clamp_page`/`page_bounds`/`pager_summary`/`prev_page`/`next_page`), the group-header
count and the confirm-dialog gate (`needs_typed_confirmation`/`can_confirm_action`) all live in
`util.rs` for this reason — the pager arithmetic had already caused a "98% of groups unreachable"
bug while sitting inline in `tables.rs`.

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
  - **Tenant-keyed, method-gated access.** `last_result` is **private** and stamped with the tenant
    (`Mutex<Option<(TenantKey, QueryResult)>>`, `TenantKey` = instance URL + client id). Never touch it
    directly — write via `state.store_last_result_if_current(token, result)` and read via
    `state.with_current_result(|r| …)`, which compare the stamp at read time, so a result from a
    different tenant reads as a miss. The whole-fleet input caches (devices/current/lookups) carry the
    same stamp. This makes the `clear_lookups_cache` / `clear_last_result` calls (sign-out, instance
    change) belt-and-suspenders for correctness — a forgotten one can't leak a prior tenant's rows; they
    remain only to reclaim memory promptly and to wipe rows on an explicit same-tenant sign-out.
  - **The write is generation- and tenant-gated (load-bearing).** `query_patches` claims a
    `QueryToken` via `state.begin_query()` **before** any fetch and redeems it at the store. Two
    things ride on that ordering. Overlapping queries (an auto-refresh tick during a manual Run) are
    ordered by *start*, not by completion, so the run the frontend renders is the run whose rows are
    cached — otherwise the visible table and the rows behind paging/export come from different
    queries. And the tenant stamp is taken from the token, i.e. the tenant the query was *fetched*
    under; a whole-fleet fetch runs for minutes, so stamping at write time could file the old
    tenant's rows under the new one — the one way the tenant check can be *wrong* rather than merely
    miss. A superseded or tenant-drifted result is dropped, not stored.
  - **The three drop reasons are distinct, and the caller must treat them differently.**
    `store_last_result_if_current` returns a `StoreOutcome`, not a bool. `Superseded` is invisible to
    the operator and the frontend already discards the response itself (`run_query` compares
    `query_seq` after the await and drops a superseded one, while still clearing its own busy flag),
    so the summary is still returned. `TenantChanged` and `Poisoned` are **errors**:
    `commands::patches::summary_for` refuses to hand back a renderable summary, because the frontend
    has no equivalent guard — `query_seq` counts runs the frontend *starts*, and switching instance
    never bumps it, so returning the summary painted the previous tenant's rows over the new tenant's
    empty cache while paging and export read the miss. Keep the rule: return a summary only when the
    rows behind it are readable.
  - **A tenant switch also clears the frontend.** `save_settings` reports `tenant_changed` on its
    `SettingsView` (both halves of the tenant key — instance *and* client id), and
    `apply_settings_view` calls `clear_results()`. Without it the previous tenant's rows stayed
    rendered against a cache that had already been dropped — the same divergence one layer up.
  - **The three paging commands all return empty on a cache miss**, never an error. A miss is a
    normal transient (tenant switch, sign-out, superseded query); the frontend already renders its
    own empty state from the absent result.
  `get_patch_rows` also takes an optional `sort` (`rows::RowSort`) and re-orders **per request** via a
  ref-sort in `rows::page_rows` — the cached rows themselves are never reordered; their canonical
  severity/org/device order feeds the export and the summary's inline first page.
  - **Grouping is backend-side too, for the same reason.** The Patches tab's *By device* / *By patch*
    modes go through `get_patch_groups` (headers + total, `rows::group_page`) and
    `get_patch_group_members` (one group's rows, `rows::group_member_page`). The frontend only ever
    holds one page, so it cannot group a fleet it has never seen — never regroup `page_rows`
    client-side. Group headers carry **no** members: a by-patch group can span the whole fleet, so
    members load on expand, capped at `GROUP_MEMBER_LIMIT`. `rows::group_key` is the identity the
    frontend echoes back, so no per-request state is kept backend-side and a stale key matches
    nothing. `demo.rs` mirrors `group_key`/`build_groups` by hand for the browser demo — keep the
    two in step.
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
  (`fleet_devices_cache`, `DEVICE_TTL` ~15 min since devices change rarely; the current patches in
  `fleet_current_os` / `fleet_current_sw`, refreshed on `force_refresh` or past
  `CURRENT_PATCHES_TTL`). `run_query` then scopes them to the
  selected identity facets (org/location/role/class) **client-side** via `FilterParams::device_allowed`
  — so changing org/location/role/type/severity re-filters the cache with **no** round trip. This is
  why `query_patches` takes the cached devices/current as *futures* (concurrent cold fetch) and why
  `device_filter` no longer exists.
  - **The two current-patch families are cached separately, and only the requested one is fetched.**
    `fleet_current_patches` takes `include_os` / `include_sw` straight from the query's `PatchType`; a
    family that wasn't asked for is neither fetched nor returned (`run_query` discards it anyway). They
    are separate slots because the families are wildly asymmetric — a whole-fleet third-party feed runs
    to six figures and is usually the largest fetch in the query, so fetching it for an `Os` query cost
    ~80 serial cursor pages of data that was then dropped, *and* made it the critical path (an OS-only
    query took about as long as an ALL query). Widening `Os` → `All` still reuses whatever is warm.
    Don't merge them back into one pair.
  - **The stores are epoch-gated, and the fetches are single-flight (load-bearing).** A whole-fleet
    fetch runs for minutes, so a mutating action's `invalidate_current_patches()` routinely lands
    while one is in flight. The store used to be unconditional, so the in-flight fetch wrote its
    pre-action rows straight back and `CURRENT_PATCHES_TTL` restarted on them — the tenant stamp
    cannot catch this, it is the same tenant. `devices_epoch` / `current_epoch` are sampled before
    the fetch and re-read **under the slot lock** at the store; the invalidators bump the epoch
    *before* clearing the slot, so the two orderings both lose the write. Separately, each cache has
    a `tokio::Mutex` fetch gate (mirroring `AuthState::refresh_lock`) with a re-check after
    acquiring: queries overlap by design, and on a cold cache both callers used to page the entire
    inventory / third-party feed independently. Per family, so an OS-only query never waits on an
    in-flight third-party fetch.
  - **`force_refresh`** (camelCase `forceRefresh`, the auto-refresh tick / manual ↻) trades
    `CURRENT_PATCHES_TTL` for `FORCE_MIN_INTERVAL` to pull fresh patch state mid-patching; a normal Run
    query leaves it false. The floor is enforced **backend-side on purpose**: the frontend cadence is a
    hint, and unbounded `force` made the whole cache decorative on the one path that runs unattended for
    hours. The frontend also skips a tick while `document.hidden` (`api::document_hidden`).

  Install history is **not** prefetched — it's fetched fresh per query, scoped
  server-side by `patch_filter` + status-pushed-down (too large to cache). The summary carries
  `data_fetched_at` (when the patch data was last fetched, distinct from `generated_at`) for the UI's
  "patch data as of …" label. The whole-fleet caches are tenant-scoped, so `clear_lookups_cache` drops
  them too. **Scoping borrows, never clones.** `run_query` filters the cached `Arc`s into
  `Vec<&Patch>`, and the rollups (`pending_counts`, `build_compliance`, `build_compliance_by_os`,
  `build_severity_by_org`, `build_age_buckets`) plus `PatchSource.patches` all take `&[&Patch]`.
  A whole-fleet third-party feed runs to six figures and each `Patch` owns **seven**
  `Option<String>`s, so cloning the scoped subset — and again into `all_current` — cost millions of
  allocations per query for data the cache already owns and outlives. Keep new rollups on
  `&[&Patch]`; don't reintroduce an owned `Vec<Patch>` to make a signature more convenient.

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
  arrived. **Native** (public) clients have **no** secret; **Web** (confidential) clients do —
  the app supports both, so don't hardcode either.
  - **Scope is conditional, and the refresh grant never re-sends it (load-bearing).**
    `scope_for(actions_enabled)` picks `monitoring offline_access` or
    `monitoring management offline_access`; `settings.actions.enabled` (default **false**) is
    what flips it, which is why adding the write path didn't break existing installs. The
    refresh grant does **not** send `scope`, so an install that signed in before actions were
    enabled keeps its read-only grant silently and every write 403s. `AuthState::management_grant()`
    detects this from the token response's `scope` (RFC 6749 §5.1, self-healing on each refresh)
    with a JWT-claim fallback; `None` means *unknowable*, not *denied*, and the UI words the two
    differently. `commands::auth::reauthorize` drops the keyring refresh token **first** so the
    browser flow must issue a fresh grant.
  - **In-memory before keyring, and only the token that got the 401 is invalidated.**
    `store_tokens` assigns `inner.tokens` **first** and downgrades a keyring write failure to a
    warning. The server has already rotated the grant by then, so propagating the error discarded a
    valid token set and the next attempt replayed the consumed refresh token into `invalid_grant`,
    which clears the credential — a transient locked keychain became a forced interactive sign-in.
    Degrading to "no persistence this session" is correct: the access token is in-memory only anyway.
    Relatedly, `invalidate_access_token(&stale)` takes the token that actually got the 401 and
    no-ops unless it is still the current one; a query fans out many concurrent requests, so a
    lagging 401 answering a *replaced* token used to mark the fresh one stale and chain into
    redundant grants.
  - **The callback listener loops over connections.** `wait_for_callback` accepts repeatedly and
    answers anything without `code`/`state`/`error` with a 404, with a per-socket read timeout.
    Handling exactly one accept meant a browser preconnect, favicon fetch or port probe consumed the
    sign-in — the documented "a hung sign-in usually means the callback never arrived" symptom.
  - **The refresh is single-flight, and only `invalid_grant` clears the credential (load-bearing).**
    A query deliberately fans out many concurrent API calls and each one calls `access_token()`
    first, so without a guard they all observe the same stale token and each POSTs the same
    `refresh_token` — last-writer-wins on both the keyring and the in-memory set. `access_token()`
    therefore takes `refresh_lock` (a `tokio::Mutex`) and re-checks under it, so concurrent callers
    await one grant. That composes with the error arm: `refresh_grant_is_dead` clears the stored
    refresh token **only** on a 400/401 whose OAuth `error` is `invalid_grant`. Clearing on any
    non-2xx (the old behavior) meant a 429, a 5xx or a captive-portal page forced an interactive
    re-login — and under refresh-token rotation the loser of a refresh race erased the credential
    the winner had just stored. Deliberately **not** "any 4xx": 429 is a retry-later status.

- **Write path (patch actions) — load-bearing rules.** The feature is opt-in
  (`settings.actions.enabled`, default false) and every command re-checks
  `require_actions_enabled` — a stale frontend must not be able to widen the blast radius.
  - **There is no per-KB apply endpoint, so there are two apply paths and the UI names both
    (load-bearing).** `/device/{id}/patch/{os,software}/apply` installs everything approved on
    the device and cannot be told which patches to install. Targeting specific patches is
    possible **only** via a library script that accepts a target list. Those are different
    mechanisms with different blast radii, so they are different `ActionKind`s —
    `OsPatchApply`/`SoftwarePatchApply` ("Apply all …") vs
    `OsPatchRemediate`/`SoftwarePatchRemediate` ("Apply selected …") — grouped under separate
    headings in `ACTION_GROUPS` (`web-rs/src/app/actions.rs`). Presenting them as one "Apply"
    button was a real hazard: ticking one row under *By patch* grouping and pressing Apply
    installs the device's whole approved backlog, and nothing said so. `plan()` now warns on the
    native kinds and names the targeted counterpart (`untargeted_counterpart` /
    `targeted_counterpart`). Don't collapse the pairs back into one action.
    - The remediation script ids live in `settings.actions.{os,software}_patch_script_id` and are
      resolved **backend-side** from Settings (`actions::remediation_script_id`), never taken from
      the request — the kind carries guardrails a hand-picked `Script` doesn't. An unset id is a
      `plan()` blocker, and so is an empty target list (a script with an empty allow list reports
      success having installed nothing). `AutomationScript::accepts_kb_allow_list` still gates the
      per-KB checkbox on the hand-driven `ScriptPicker` path.
    - **The parameter encoding is chosen by kind.** `build_parameters` sends `kbAllowList=`
      (comma-separated KBs) for OS and `productAllowListB64=` (base64 of titles joined by `|`)
      for software, because NinjaOne splits `parameters` on **spaces** and product titles contain
      them. The software arm was dead code until `SoftwarePatchRemediate` existed: the only caller
      composed for `ActionKind::Script`, which falls to the `kbAllowList` arm, so a software
      remediation script was handed a KB list — and third-party patches carry no KB, so it was
      always empty.
  - **Selection is per patch row; dispatch is per device, with per-device targets
    (load-bearing).** `DeviceSelection.patches` maps each ticked row's `patch_key` → a
    `SelectedPatch { kb, name, is_os }`, and a device enters the selection with its first ticked
    row and leaves with its last. Ticking a row must **not** tick the device's other rows: it once
    did, which swept every KB on the device into `kbAllowList` and made the one path capable of
    per-patch targeting unable to receive a subset.
    - A dispatch sends each device **only the patches ticked on it**
      (`util::targets_by_device` → `ActionRequest.device_targets` →
      `commands::actions::per_device_parameters`, a `BTreeMap<i64, String>` carried on
      `DispatchContext`). This covers **every** path that sends an allow list — both remediation
      kinds *and* the script picker's "Target only the selected KBs". There is no batch-wide
      `targets` field any more: it handed every device the union of the selection, which is
      invisible in a dialog showing one parameter string. Don't reintroduce one; a genuinely
      uniform string is what the verbatim `parameters` field is for, and it is honored only on the
      `Script` path where the operator can actually type it.
    - Devices with nothing ticked *of that family* are dropped from a remediation's `device_ids`
      entirely rather than dispatched with an empty list. A hand-picked `Script` keeps them (the
      operator chose them and the script may not need a list), and `build_plan` warns, naming them
      via `untargeted_names` / `summarize_names`.
    - What the *native* Apply does on those devices is still all-or-nothing — that's the endpoint,
      not the selection model — so don't "fix" that gap by widening selection again.
    - Third-party patches carry no KB (the software feed has no `kbNumber`), so they are targeted
      by **product title** instead; an OS remediation silently skips them and vice versa, mirroring
      the asymmetry of the two feeds.
  - **`ReplaySafety::ActOnce` on every POST.** `request_raw`'s timeout arm would otherwise
    replay the body and re-run the action; 429/401 still replay (the gateway rejected before
    the device queue). A timed-out dispatch becomes `JobState::Unknown` — polled, never
    auto-retried.
  - **Confirm tokens are payload-bound and single-use.** `plan_action` hashes **everything that
    reaches NinjaOne or that the guardrails read** — kind ‖ sorted device ids ‖ script ref ‖
    **resolved** script ‖ per-device parameters ‖ run_as ‖ reboot choice ‖ reboot mode ‖
    include_offline ‖ override_window ‖ dry_run — into a 5-minute token; `run_action` re-plans
    from scratch and re-checks the hash. The parameters are hashed as
    `canonical_parameters` — every device's own string, bound to its id — so re-ticking one row on
    one device invalidates the approval; and the *resolved* script is hashed separately because
    for a remediation kind it comes from Settings rather than from the request, so an id edited
    while the dialog is open would otherwise run a different script under the same approval.
    `canonical_parameters` **length-prefixes each value**. The `0x1f` separator discipline below is
    enough for fields the toolkit composes, but a parameter string can be *typed by hand* in the
    script picker, so `{1: "a\u{1e}2=b"}` rendered identically to `{1: "a", 2: "b"}` — two
    different dispatches sharing one approval. Editing the selection after the dialog opened invalidates the approval
    rather than widening it. `request_hash` **destructures `ActionRequest` exhaustively**, so a new
    field is a compile error there rather than a silent omission — which is exactly how
    `include_offline`, `override_window` and `run_as` came to be missing (the first two gate
    `plan()`'s offline warning and maintenance-window blocker; the third is the execution identity).
    Fields are separated by `0x1f` so two different requests can't concatenate to one hash input.
  - **There is one dispatch surface, and the run options are shared (load-bearing).** Everything
    dispatches from the `ActionBar` on the Patches tab, next to the selection it targets; the
    `ScriptPicker` is folded into it behind a `<details>` and the Jobs tab is history only.
    `Run as`, `Restart the device after installing` and `Dry run` are rendered **once** and reach
    every `runs_a_script()` kind — they mean the same thing for a remediation install and a
    hand-picked script, and duplicating the controls across two tabs while they wrote the same
    signals meant ticking "Dry run" in the Jobs tab silently changed what an Apply button did.
    Each options row carries a label naming the actions it reaches: the native endpoints take no
    parameters, have no preview mode and run as NinjaOne's agent, so an unlabelled "Dry run" beside
    them reads as protection they cannot give.
  - **Guardrails live in `actions::plan`**, which is pure with an injected clock. Adding one
    means extending `blockers`/`warnings` there, not adding a dialog. The one exception is the
    `dry_run` check, which is *also* asserted at the dispatch site in `run_action` — defense in
    depth, so a new `ActionKind` whose `supports_dry_run()` is wrong can't send a real mutating
    POST while the UI says "Dry run".
  - **After a mutating action, call `invalidate_current_patches()`** (and
    `invalidate_fleet_devices()` after a reboot) — `clear_lookups_cache()` is too blunt, and
    the 120 s current-patch TTL would otherwise serve pre-action data. `last_result` is
    deliberately *not* dropped; the frontend raises a stale-results banner instead.
  - **Job state is tenant-stamped** in `AppState.jobs`, mirroring `last_result` — a tenant
    switch reads as a miss. The poller is single-claim (`try_claim_job_poller`) and emits
    `action:progress` (no capability change needed; `core:event:default` already covers it).
    It retires via `release_job_poller_if_idle()`, which re-checks for pending jobs **and**
    clears the claim flag under the jobs lock. Dispatch appends its jobs before calling
    `try_claim_job_poller`, so a batch landing during shutdown is either seen (the poller keeps
    going) or strictly after the release (its own claim succeeds). Releasing unconditionally left
    jobs dispatched in that gap with no poller at all.
  - **NinjaOne v2 has no script-output endpoint.** A job resolves from `/activities` only, so
    surface the exit code plus the activity/series correlator.

- **NinjaOne API client — reuse the shared retry + pagination.** Every call goes through
  `NinjaApiClient` (`api/mod.rs`): `{base}/api/v2{path}`, bearer auth, retry on timeout / connect
  failure / **5xx** / 429 (honors `Retry-After`) / 401 (forces a token refresh). `get_paginated`
  handles **both** a bare
  JSON array **and** the `{ results, cursor }` envelope, where `cursor` may be a string or a
  `{ name, offset, … }` object; it stops when a page returns 0 rows even if the server echoes a
  stale token. Don't hand-roll a second reqwest/cursor loop.
  - **The retry policy is a pure function.** `retry_for(status, replay, attempt, retry_after)`
    returns `Retry::{No, Wait, Reauth}`, and `decode_response` handles the body — extracted from a
    ~300-line `request_raw` so the policy can be tested without a server. Its arms are below.
  - **An unreadable `cursor` is an error, not end-of-pages.** `next_cursor` returns
    `Result<Option<String>>` and bails on a shape it cannot interpret (an object with no usable
    `name`, a number, an array). It is only consulted after a page that *returned rows* — the caller
    checks `page_len == 0` first — so treating an unknown shape as "finished" ended the fetch
    mid-fleet and handed back a partial result that looked complete, understating every compliance
    number derived from it. This mirrors the `results`-not-an-array arm, which has always bailed.
  - **The 5xx and connect arms are `Idempotent`-only.** A reporting pull is dozens of *sequential*
    cursor pages, so a gateway 502 on a late page used to discard every page already accumulated —
    5xx is the most common transient failure on that path, far more so than 429. But a 5xx on an
    acting POST is exactly the ambiguity `ReplaySafety::ActOnce` exists for (the gateway may have
    failed *after* the job reached the device queue), so writes still fail through to
    `JobState::Unknown` and are polled, never replayed. 429/401 stay replayable for both.
  - **reqwest's default features are off, so every one it drops must be re-added explicitly**
    (`src-tauri/Cargo.toml`). `default-features = false` is there to pin TLS to rustls, but it also
    drops `charset`, `http2`, and `system-proxy` — and `gzip` was never on. Uncompressed six-figure
    JSON feeds and a fresh TLS handshake per concurrent fetch were both silent consequences of that
    one line. If you touch the feature list, keep `gzip`, `http2`, `system-proxy`, `charset`.

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

- **There is no patch release date in the NinjaOne API (load-bearing).** Grep the spec: `releaseDate`
  appears **zero** times. `DeviceOSPatch` / `DeviceSoftwarePatch` carry only `installedAt`
  ("Installation attempt timestamp") and `timestamp` ("Date/Time when data was collected/updated");
  the non-`Device` `OSPatch`/`SoftwarePatch` variants carry neither. `Patch::collected_timestamp`
  (alias `timestamp`, read via `first_seen_at()`) is therefore **detection time, not publication
  time**, and everything derived from it — `PatchRow.first_seen_ts`/`first_seen_date`, the SLA
  `aged_critical` rollup, `build_age_buckets`, and the `detected_within_days`/`detected_after`/
  `detected_before` filter window — measures *how long we have known about the patch*. The UI says so
  ("First seen", "Pending past SLA", "Pending patch age (since first seen)"); keep the naming honest
  if you touch these. This used to be a field named `release_timestamp` aliasing a `releaseDate` that
  never binds, so the SLA rollup compared *now* against an always-recent timestamp and reported ~0
  breaches on any fleet — and the wiremock fixtures fed `releaseDate`, so CI proved only that the
  aliasing worked. **Fixtures must emit `timestamp`.** Undated pending patches get their own
  `Unknown` age bucket rather than inflating `180+ days`; they still count as aged in the SLA rollup
  (`unwrap_or(true)` — can't prove recent).

- **Severity: NinjaOne sends two vocabularies on one field (load-bearing).** The feeds mix
  uppercase MSRC values (`CRITICAL`/`IMPORTANT`/`OPTIONAL`/`NONE`) with lowercase engine values
  (`critical`/`security`/`optional`/`recommended`/`unknown`), and third-party patches carry the
  grade in `impact`, not `severity` (aliased onto `Patch::severity`). `security` and `recommended`
  are NinjaOne **classifications, not urgency grades**, so `Severity` models them as their own
  variants — bucketing them into MSRC levels would misreport them in the export and charts.
  Anything `from_raw` fails to map becomes `Unknown` (rank 0), which both sinks it below every
  other patch in the severity sort **and** makes it unreachable from the severity facet; that is
  why an unmapped value reads as "those patches don't exist". `SEVERITY_OPTIONS`
  (`web-rs/src/app.rs`) must therefore cover the whole vocabulary including `UNKNOWN`. Ranks are
  ordered so `Security`/`Recommended` fall **below** `Important`, keeping them out of the
  `rank() >= Important.rank()` compliance/SLA rollups. Adding a value means: `from_raw` + `label`
  + `rank` (`model.rs`) → the `SeverityCounts` field **and** `SeverityCounts::BANDS` **and** its
  `AddAssign` (`rows.rs`) → the `web-rs/src/types.rs` mirror →
  `SEV_BANDS`/`sum_severity`/`sev_count`/`severity_segments` (`charts.rs`) → `SEVERITY_COLORS`
  (`report.rs`) → `sev_class` **and** `sev_ordinal` **and** `severity_raw` (`util.rs`) →
  `SEVERITY_OPTIONS` → the CSS → `demo.rs`. The CSS end of this is now guarded: the eight band
  colors are `--sev-*` / `--sev-*-fg` custom properties defined once on `:root`, and the three rule
  families (`.sev-*`, `.chart .seg-*`, `.chart-swatch.seg-*`) `var()` them rather than restating hex
  values. `severity_css_defines_every_band` (`util.rs`) compiles `styles.css` in with `include_str!`
  and fails if a band is missing any of the four — CSS cannot give a compile error, so that test is
  the substitute. The three families still exist for a reason: the middle one sets `fill` and is
  scoped to `.chart`, so it does nothing for a legend `<span>`.
  - **`rows::SeverityCounts::BANDS` is the canonical enumeration on the counts side.** It pairs each
    label with a typed accessor, and `total()`, the HTML report's chart, its legend and its
    denominator all derive from it — so they cannot disagree about how many bands exist.
    `report.rs` now contributes only `SEVERITY_COLORS`, whose length is tied to `BANDS.len()` by its
    array type (a band without a color is a compile error). The old `severity_value` matched bands
    by **string label** with a `_ => counts.unknown` catch-all, so a renamed band silently reported
    Unknown's count and double-counted it into the total. `total_severity_is_the_sum_of_its_bands`
    (`rows.rs`) fails if a field is added to the struct but not to `BANDS`.
  - **`rows::TableColumn<T>` is the shared table definition.** Every table rendered from a cached
    `QueryResult` — `FailureGroup::COLUMNS`, `DeviceSummary::COLUMNS`, `ComplianceBucket::COLUMNS`,
    `OsCompliance::COLUMNS`, plus `export.rs`'s own `DETAIL_COLUMNS` — pairs each header with the
    accessor that fills it, so a column is one declaration rather than two lists agreeing by
    convention. `export.rs` renders all five through one `write_sheet` and contributes only the
    width arrays, each length-tied to its `COLUMNS.len()`; `report.rs` renders through one
    `write_table`. Both had already diverged: the report dropped `Patch Type` from the failures
    table, and hardcoded the reboot table's headers as "Role"/"Pending patches" against the
    workbook's "Device Role"/"Pending Patches".
  - **This list was previously incomplete, and every site it omitted had silently drifted:**
    `report.rs` summed six of eight bands by hand (a `security`/`recommended`-only backlog printed
    "No pending patches"; a mixed one overflowed the viewBox), the two `.chart-swatch` rules were
    missing (blank legend squares), `.sev-optional`/`.sev-unknown` were missing (both collapsed into
    `.sev-none`, so "low priority" and "unmapped" rendered identically), and `sev_ordinal` ranked
    both classifications *below* `Optional`. Prefer deriving over enumerating where you can —
    `write_severity_chart` now sums via `SEVERITY_BANDS` so its denominator cannot diverge from the
    segments it draws, which removes one hand-maintained site from this list entirely.

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

- No abstraction, configuration, or generality for hypothetical futures (YAGNI).
- Comments explain *why*, not *what*.
- Dependencies are a cost; prefer std lib and existing crate deps.

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
9. **Manifest versions** *(GitHub-side)* — the `versions` job in `ci.yml` checks that
   `tauri.conf.json`, `src-tauri/Cargo.toml` and `web-rs/Cargo.toml` carry the same version on
   **every PR**. `release.yml`'s guard also compares them against the tag, but only under
   `if: startsWith(github.ref, 'refs/tags/')` — i.e. after the tag and its irreversible release run
   have been pushed. The two crates share no workspace, so this is bumped by hand and the manifests
   co-change in ~23 of every 300 commits.

CI runs the same gates in the same order (`ci.yml`'s frontend job runs `web-check`, `web-clippy`,
`web-build`, `web-test`) — keep it that way; a CI sequence that quietly differs from the documented
one is how the two drift.

For behavior changes not provable by a unit test, run `just dev` and exercise the view.

## Keeping this file up to date

When editing these surfaces, update the matching section here:
crate/dir/module changes → **Repo map**; toolchain/MSRV/edition → **Quick Reference**;
`justfile` recipes → **Canonical commands** + **Verification playbook**;
new command / IPC arg shape / cache / auth / filter / CSP → **Common patterns** + **Conventions & gotchas**;
CI gate or `tauri.conf.json` bundle → **Verification playbook**.

The staleness hook (`agents-md-staleness-check.sh`) reminds you if you forget.
