---
name: debug
description: Debug issues in the NinjaOne Patch Toolkit (Tauri + Leptos WASM app). Use when the user says "debug X" where X is a symptom (e.g., "sign-in hangs", "patches not loading", "export is empty", "action buttons greyed out", "job stuck on Running", "WASM build error"), or asks for help diagnosing a problem.
argument-hint: "[symptom] — e.g., 'sign-in hangs', 'export is empty', 'job stuck'"
---

# Debug — diagnose Tauri + Leptos WASM issues

Start with a hypothesis from the symptom, then check the layer it points at. The rationale
behind each subsystem lives in `docs/design/<domain>.md`; read the matching note before
changing anything there. `docs/TROUBLESHOOTING.md` is the operator-facing list of the same
symptoms — check it first, it may already name the cause.

## 0. Clarify the symptom

- Which surface: sign-in/auth, filters, patch list (paging / grouping / sorting), Compliance /
  Needs-Reboot / Failures tabs, Excel or HTML export, patch actions + jobs, settings/presets,
  auto-update?
- Native (`just dev`) or the browser demo (Pages)? The demo has no backend: export, sign-in and
  actions are inert there by design (`api::is_tauri()`).
- Any error toast (`UiError.message`), terminal log, or browser console output?

## 1. Backend (`src-tauri/`)

- `just clippy` then `just test` — the wiremock integration tests cover the API client and the
  query path; a failing test usually names the layer.
- Query path: `commands/patches.rs` (`run_query` → `assemble_result`) → `rows/` (join,
  compliance, rollups, groups, scope) → `state.rs` (the tenant-keyed, generation-gated cache).
  Empty rows with a visible table = the frontend rendered a summary the cache dropped; see
  `docs/design/query-cache.md`.
- API client: `api/mod.rs` (retry policy `retry_for`, `parse_page`, cursor paging) →
  `api/{devices,patches,lookups,actions,activities}.rs`. Verify endpoint shapes against the spec,
  never from memory (`docs/design/api-client.md`).
- Filter: `filter.rs` — device scope is client-side (`PreparedFilter::device_allowed`), only the
  install-history `df` is server-side (`docs/design/filter.md`).

## 2. Frontend (`web-rs/`)

- `just web-clippy` (wasm) and `just web-test` (host-target pure helpers).
- `src/api.rs` — every wrapper is an `ipc!(...)` declaration; the command string and the camelCase
  arg keys derive from the wrapper's own name and parameters, so a mismatch is a typo in one place.
- `src/types.rs` — the hand-mirrored IPC types; a missing field deserializes as an error toast
  "decode <cmd>". `rows::tests::serialized_shapes_carry_every_frontend_required_key` pins the keys.
- `src/app/state.rs` (signals + run/selection/session logic) and the view modules under
  `src/app/`: `tables.rs`, `filters.rs`, `controls.rs`, `actions.rs`, `settings.rs`, `charts.rs`,
  `header.rs`, `update.rs`, `modal.rs`, `toaster.rs`. Pure helpers live in `src/app/util/` and are
  the only frontend code with tests.

## 3. Auth (`src-tauri/src/auth.rs`, `docs/design/auth.md`)

- **Sign-in hangs:** the PKCE callback never arrived on `callback_port` (default `11434`); the
  browser must reach `http://127.0.0.1:<port>`. The listener loops over connections, so a
  preconnect or favicon fetch does not consume the sign-in.
- **Scope:** `monitoring offline_access`, plus `management` only when `settings.actions.enabled`.
  The refresh grant never re-sends scope, so an install that signed in before enabling actions
  keeps a read-only grant and every write 403s → **Re-authorize** (`commands::auth::reauthorize`
  drops the refresh token first).
- **Not signed in after restart:** keyring read failed or empty. Only `invalid_grant` clears the
  stored credential; a 429/5xx must not.
- **Native vs Web client:** Native has no secret; "secret required" means Settings is mismatched.

## 4. Patch actions + jobs (`src-tauri/src/actions.rs`, `commands/actions.rs`, `docs/design/actions.md`)

- **Buttons greyed out:** `settings.actions.enabled` is false, no management grant
  (`AuthState::management_grant()` — `None` is *unknown*, not denied), nothing selected, or a
  `plan()` blocker (maintenance window, blast-radius cap, org-span cap, unset remediation script
  id, empty target list).
- **"Apply selected" installed nothing / everything:** the native apply endpoints are
  all-or-nothing; only the library-script remediation kinds take per-device targets
  (`per_device_parameters`). Check which `ActionKind` was dispatched.
- **Job stuck on Running / Unknown:** jobs resolve from `/activities` only. `newerThan` is an
  activity *id*; the time floor is client-side. A timed-out dispatch is `Unknown`, polled, never
  replayed (`ReplaySafety::ActOnce`).
- **"Not confirmed":** the confirm token is payload-bound; any edit to the selection or options
  after the dialog opened invalidates it (`request_hash`).

## 5. IPC boundary

- Registered in `generate_handler![]` (`src-tauri/src/lib.rs`)? Declared with `ipc!` in
  `web-rs/src/api.rs`? The `command-parity-check.sh` hook reports either gap after an edit to the
  chain; `.claude/hooks/test.sh` proves the hook itself works.
- Every mutating handler calls `require_actions_enabled`; the test
  `every_mutating_command_checks_that_actions_are_enabled` derives the list from source.

## 6. Output format

```
# Debug Report — Export Is Empty

## Symptom
Excel export writes a workbook with only headers.

## Hypothesis
`export_patches_xlsx` reads the cached result; the last query was superseded or the tenant
changed, so the cache read as a miss while the frontend still rendered the old summary.

## Evidence
- `src-tauri/src/commands/export.rs` takes `current_result_handle()`.
- `src-tauri/src/commands/patches.rs::summary_for` returns an error on TenantChanged/Poisoned.

## Next steps
1. `just dev`, switch instance, run a query, export.
2. If still empty → log `StoreOutcome` at the store site.

## Files changed
- (none yet)
```

## Common symptoms and quick checks

| Symptom | Quick check | Likely culprit |
|---------|-------------|----------------|
| Sign-in hangs | port reachable? firewall? | PKCE callback never arrives (`auth.rs`) |
| Everything 403s after enabling actions | token scope | read-only grant kept by refresh → Re-authorize |
| Not signed in after restart | keyring entry present? | refresh token not stored/read (`auth.rs`) |
| Patches list empty | statuses selected? scope ids stale after tenant switch? | `filter.rs` scope / `clear_session` |
| FAILED query returns nothing | install window days? | `Failed` routes to install history (`commands/patches.rs`) |
| Next page blank under "Rows 101–200 of N" | tenant/sign-out since the query? | cache miss; frontend not cleared (`docs/design/query-cache.md`) |
| Export empty | did a query run on this tenant? | `last_result` miss |
| Compliance % too high | offline / non-patchable counts in the scope note | `rollup_device` population (`rows/compliance.rs`) |
| Action buttons greyed out | actions enabled? management grant? blockers? | `actions::plan`, `management_grant` |
| Job stuck / Unknown | `/activities` type filter, `newerThan` id | `api/activities.rs`, poller |
| WASM build error | `just web-clippy` | server dep pulled into `web-rs` |
| Command not found in frontend | `.claude/hooks/test.sh`, `api.rs` | parity gap |

## Failure handling

- `just verify` passes but the issue persists → runtime/logic bug; reproduce with `just dev`.
- `just verify` fails → fix the failing gate first.
- WASM-only → Trunk output (`web-rs/dist/`), browser console, `console_error_panic_hook` output.
