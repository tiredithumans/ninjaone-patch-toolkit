# Frontend, IPC boundary, and the testing rule

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/lib.rs`, `src-tauri/src/commands/`, `src-tauri/src/error.rs`,
`web-rs/src/api.rs`, `web-rs/src/types.rs`, `web-rs/src/app/`, `web-rs/src/demo.rs`,
`src-tauri/tauri.conf.json`.

## Tauri commands

`#[tauri::command] fn` → `State<'_, AppState>` first → `Result<T, UiError>`. `UiError`
serializes to `{ message }`, which the frontend renders in a toast (map errors with
`.map_err(UiError::from)`). Must be in `generate_handler![]` **and** have an `invoke(...)`
wrapper in `web-rs/src/api.rs`.

**`async` is the default, not a requirement.** A handler that only reads or writes in-process
state and never `.await`s anything — `auth_status`, `list_jobs`, `clear_jobs`, the settings
getters, `list_node_classes` — is a plain `pub fn`, and 8 of the 26 handlers are. Making one
`async` to satisfy the shape buys nothing; making a handler that *does* I/O synchronous blocks a
runtime worker (see [concurrency.md](./concurrency.md)). The contract that always holds is the
argument order, the `Result<T, UiError>` return, and the two registrations.

**A mutating handler must call `require_actions_enabled`** — see
[actions.md](./actions.md) and the test
`every_mutating_command_checks_that_actions_are_enabled`.

## IPC arg shape — keys match Rust fn parameter names (camelCase)

The frontend wrapper builds an arg object whose keys equal the handler's parameter names. A
handler taking `args: PatchQueryArgs` is invoked with `{ args: {...} }`; one taking `org_id: i64`
is invoked with `{ orgId: ... }`. Arg structs use `#[serde(rename_all = "camelCase")]`. Renaming
a parameter is a wire-format change — update both sides.

The `ipc!` macro in `web-rs/src/api.rs` makes both hold by construction: `ipc!(name(arg: T, …)
-> Ret)` generates the camelCase arg struct and the `invoke` call, so the arg keys equal the
wrapper's parameter names and the command string equals the wrapper's name. A wrapper
deliberately named differently spells the target out:
`ipc!(export_patches as "export_patches_xlsx", () -> Option<String>)`.

Args are serialized with `serde_wasm_bindgen::Serializer::json_compatible()`. The default
serializer turns a `HashMap` into a JS `Map`, which Tauri's IPC flattens to `{}` — that is how
`ActionRequest.device_targets` arrived empty. `args_of` returns a `Result`, so a serialization
failure is reported rather than sent as `undefined`.

## camelCase ↔ snake_case across IPC

Backend arg/result structs sent to/from the frontend carry `#[serde(rename_all = "camelCase")]`;
`web-rs/src/types.rs` mirrors them. NinjaOne API JSON (e.g. `systemName`, `nodeClass`) is
deserialized inside the backend models — that's separate from the IPC wire format.

## WASM gating

`web-rs` compiles to `wasm32-unknown-unknown` and is a **separate crate**. Server deps (tokio,
reqwest, keyring, rust_xlsxwriter) belong in `src-tauri` only — never pull them into `web-rs`.
Shared logic that must run in both is duplicated as plain types, not shared via a crate.

## CSP governs the webview, not backend egress

`connect-src` in `tauri.conf.json` is `'self' ipc: http://ipc.localhost` — the webview only talks
to the backend over IPC. **All** NinjaOne HTTP happens in the Rust backend (reqwest), so adding a
new NinjaOne region/host needs **no** CSP change. Don't add `connect-src` entries for backend
calls.

## Auto-update

`commands::update::{check_for_update, install_update}` wrap `tauri-plugin-updater`; the
frontend's `UpdateSplash` shows the release notes (changelog) and the install relaunches the app.
The updater fetches the signed `latest.json` from the GitHub releases endpoint
(`tauri.conf.json` → `plugins.updater`) — **backend egress, not subject to the CSP**. The launch
check is gated by the `auto_check_updates` setting. Packaging and signing:
[ci.md](./ci.md#auto-update-packaging).

## Frontend reactivity is closure-based (Leptos CSR)

`{move || sig.get()}` to track, `.get()` / `.with()` to read; state is `RwSignal<T>`. CSS is
plain global `web-rs/styles.css`.

**A dialog calls `modal::focus_trap()` in the closure that creates it.** `role="dialog"
aria-modal="true"` moves nothing by itself: focus stays on the opener under the overlay, so Tab
walks the covered page and Space re-invokes `open_plan` behind the dialog. The trap focuses the
container (`tabindex="-1"`, `node_ref`) on mount, wraps Tab at either end, and returns focus to
the opener in `on_cleanup` — which is why it must be created *per dialog instance* (inside the
`pending.map(...)` / `info.map(...)` closure), not once per component. `web-sys` is listed in
`web-rs/Cargo.toml` only to enable the DOM features this needs.

The corollary: the closure that creates the dialog must re-run only when a *new* dialog opens.
The device drill-down (`tables/device.rs`) keys that closure on a `Memo` of the open device id, and
renders its loading/loaded body in a nested closure — if the loaded detail arriving (or a refresh
reloading it) re-ran the outer closure, the new trap would record the outgoing dialog as its
opener and return focus to a detached node on close. Escape closes it; it has no action buttons
(dispatch stays on the one `ActionBar`).

**A table re-renders only the rows whose data changed, so focus inside it survives updates.**
Rebuilding a list re-creates its DOM and drops keyboard focus. An open group's body reads its
own `query.members` slot through a `Memo` compared by `Arc::ptr_eq`
(`util::member_entry_changed`), so a slot is always replaced with a fresh `Arc` and never
mutated in place — an in-place edit would never be seen. The Jobs table is a keyed `<For>`
that builds a row's cells once per `util::job_row_key`, so every column that can change after
dispatch (status, exit code, duration, correlators, retryability) must be in that key or the
row keeps showing the old value. The header checkboxes read `(all, some)` from one `Memo` over
nested `.with` reads (`util::rows_selection_state`), never a clone of the rows.

**An async response applies only if its request is still current.** Every page, group-header and
group-member fetch is stamped (`QueryState.view_seq`, `members_gen`) and dropped on arrival if a
newer request, a regroup, or a re-query has moved the stamp. Without it a slow sort overwrote a
newer one, and a late member fetch ticked the previous result's rows into the new selection. A
manual run that finds another in flight is queued (`util::queue_run`), never silently dropped.

## Demo mode + browser/Pages guard

The same frontend serves two contexts. Inside Tauri it talks to the backend over IPC; in a plain
browser (the GitHub Pages live demo) there is **no** backend. `api::is_tauri()` (checks
`window.__TAURI__`) gates this: `invoke` and `on_query_progress` no-op outside Tauri so an
undefined global never throws, and `App` startup branches — under Tauri it runs the
auth/lookups/settings flow; in a browser it sets `web_mode` and calls `enter_demo()`.

`web-rs/src/demo.rs` is the **only** source of sample data — pure builders (no `js_sys`/IPC), so
they host-test via `just web-test`. `enter_demo()` seeds the org/role/OS-type lookup dropdowns
from the sample and flags `demo`, but leaves the results **empty** ("Run a query to list
patches") until the user presses **Run query** — exactly like the real app. **Run query** routes
to `run_demo_query` → `demo::filtered_result(...)`, which mirrors the backend's *display*
filtering (identity/class/text facets + date windows) over the sample rows so the demo's controls
actually filter — Compliance/Reboot stay representative (narrowed only by org; the reboot list and
the device drill-down share `demo::sample_device_summary`, so their counts agree). Demo mode is
**web-only**: there is no "load sample data" affordance and the desktop release never enters it
(no auto-load → `demo` stays false and the normal auth path runs). `web_mode` also disables the
backend-only actions (sign-in, **export**).

The Pages build (`just web-build-pages`, `.github/workflows/pages.yml`) sets the subpath base
href via `--public-url` — **never** put `public_url` in `Trunk.toml`, or Tauri's relative-dist
webview breaks. Pages deploys only from `main`; backend features (queries, export, auth) are
desktop-only and intentionally inert in the hosted demo.

## Non-trivial logic does not belong in a `#[component]` body

The frontend's `just web-test` covers only the JS-free **pure helpers** (run on the host target;
the wasm build excludes the `#[cfg(test)]` module). Components and `js_sys`-backed helpers aren't
unit-tested, so `verify` still leans on `web-clippy` (which type-checks the wasm target first) for the rest of the frontend. A
`#[component]` can only be compile-checked, so arithmetic written inline inside one is unreachable
by any test.

Put such logic in the `util` module (`web-rs/src/app/util/`) as a free function and test it
there. The same rule covers `state.rs` and the `impl AppState` files under `state/` (one per
concern: `query`, `view`, `selection`, `actions`, `lookups`, `presets`), which are not component
files and have no test module — anything in them worth asserting moves to `util` rather than
staying unreachable. What lives in
`util` for this reason:

- `filter_params` (the `FilterParams` mapping behind *every* query, lifted out of
  `FilterState::current_filter`).
- `parse_clamped` / `parse_optional_id` (the settings number fields — `<input type="number">`
  treats `min`/`max` as advisory, so the clamp is the real guard).
- `action_disabled_reason` / `selection_summary`.
- The dispatch guardrails in `util/guardrails.rs`: the maintenance-window editor's time/day
  helpers and `window_summary`, `window_override_offered`, `dry_run_disabled_reason` /
  `dry_run_caveat`, and the Apply-all preview lines.
- The pieces of `state.rs` that decide *what happens*: `run_decision` (the Run guard chain, whose
  **order** is load-bearing — demo before auth, busy before both), `next_query_seq`/`is_superseded`
  (the overlapping-run stamp), and `apply_row_selection` (the selection model — a device enters
  with its first ticked row and leaves with its last, and ticking one row must not tick the
  device's others).
- `date_to_epoch` / `epoch_to_date` — plain civil-date arithmetic rather than `js_sys::Date`, so
  they host-test, and `demo.rs` shares them instead of keeping a second copy.
- The Needs Reboot tab's device selection (`apply_device_selection`,
  `prune_device_level_selection`, `build_device_action_request`, `source_disabled_reason`) and
  the Jobs tab's retry (`retry_blocked_reason`, `retry_request`, `retryable_batches`).
- The pager (`page_count`/`clamp_page`/`page_bounds`/`pager_summary`/`prev_page`/`next_page`),
  the group-header count and the confirm-dialog gate
  (`needs_typed_confirmation`/`can_confirm_action`). The pager arithmetic once caused a "98% of
  groups unreachable" bug while sitting inline in `tables.rs`.
- The operator-UX rules below: `refresh_hold`/`advance_refresh`/`countdown_label`,
  `shortcut_for`/`is_text_entry`, the `columns` helpers, `encode_view`/`decode_view`, `Theme`.
  These newer files carry their own `#[cfg(test)] mod tests` rather than growing `tests.rs`.

## Operator UX

Per-machine view conveniences — none of them configuration, so none go through
`settings.json` (see `api::ui_pref`) — and the rules each one keeps.

**Auto-refresh countdown.** A one-second ticker (`AppState::tick_auto_refresh`) advances a
wall-clock countdown (`util::advance_refresh`; ticks are throttled in the background, so counting
them drifts). `util::refresh_hold` decides, in this order, why it waits: the operator's **Pause**,
an open dialog (any `aria-modal` element, plus the Settings panel), a hidden window, a run in
flight. A run restarts the full cadence, so a manual Run also pushes the next automatic one out.
**A selection is deliberately not a hold**: a refresh already prunes the selection to rows still
listed, and after a dispatch the selection is still there exactly while the operator watches the
patches land — pausing on it would switch the cadence off when it is most wanted. Picking a
cadence lifts a pause.

**Keyboard shortcuts.** `util::shortcut_for` is the whole key map and `SHORTCUT_HELP` the help
dialog/README table (`every_documented_key_is_bound`). It stands down while typing
(`util::is_text_entry` — a focused checkbox is *not* typing, since focus stays on it after ticking
a row), under any modal, on auto-repeat and with Ctrl/Alt/Meta (Shift is allowed: `?` is
Shift+/). **No key reaches a mutating action, an export or sign-out**; `r` runs the (read-only)
query, and `[`/`]` page the Patches table through the same `go_to_patches_page` as the pager.

**Column chooser.** Stored as the set of *hidden* column ids (`util::column_id`, a slug of the
header label), so a column another build adds is visible by default and an id this build does not
know is kept and ignored. Hiding is positional CSS generated per table
(`util::hidden_columns_css`), not a filtered cell list, so adding a column touches only
`PATCH_COLUMNS` and the row markup. Device and Patch are required. Exports ignore it.

**Shareable view links.** `v1.` + base64url of a short JSON object: the preset shape
(`FilterParams`, type, statuses, install window) plus tab, grouping, sort and the instance
**host**. Never the selection, a credential or a client id
(`the_code_carries_no_selection_or_credentials`). The version sits outside the payload so a
future format is refused before parsing; decoding drops unknown statuses/severities/tabs/sort keys,
clamps numbers to what the controls accept, caps text, and `apply_view` prunes org/role/class ids
against the loaded lookups. A code from another host is held behind an **Apply anyway** banner —
its organization ids mean something else there. The web demo keeps the code in the URL fragment
(`history.replaceState`, so Back is not an undo stack of checkboxes) and applies it on load; the
desktop copies a bare code via `navigator.clipboard` — a web API, not a Tauri capability, and not
governed by the CSP — and always leaves it in a read-only field in case the clipboard refuses.

**Themes and motion.** Every colour in `styles.css` is a `:root` token (the dark palette, and the
fallback). A light palette restates *every* token twice — under
`@media (prefers-color-scheme: light)` for System, and under `:root[data-theme="light"]` for an
explicit choice — which `the_light_palette_overrides_every_root_token` enforces; a missed token
leaks a dark pastel onto white. Charts read the same tokens through classes, so they follow.
`prefers-reduced-motion` stops transitions and the indeterminate progress slide.

**Window geometry** (backend, `src-tauri/src/window_state.rs`). Its own `window-state.json`, not a
`Settings` field, so a drag never goes through `replace_settings`. Saved debounced on
move/resize (on a blocking thread) and synchronously on close; restored in `setup` before the
hidden-at-launch window is shown. `window_state::placement` keeps a saved position only when a
grab-able strip of the title bar lands on a current monitor's work area, otherwise lets the OS
place the window, and always fits the size to the monitor.
