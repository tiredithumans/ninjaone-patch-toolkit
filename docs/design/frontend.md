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
actually filter — Compliance/Reboot stay representative (narrowed only by org). Demo mode is
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
there. The same rule covers `state.rs`, which is not a component file and has no test module —
anything in it worth asserting moves to `util` rather than staying unreachable. What lives in
`util` for this reason:

- `filter_params` (the `FilterParams` mapping behind *every* query, lifted out of
  `FilterState::current_filter`).
- `parse_clamped` / `parse_optional_id` (the settings number fields — `<input type="number">`
  treats `min`/`max` as advisory, so the clamp is the real guard).
- `action_disabled_reason` / `selection_summary`.
- The pieces of `state.rs` that decide *what happens*: `run_decision` (the Run guard chain, whose
  **order** is load-bearing — demo before auth, busy before both), `next_query_seq`/`is_superseded`
  (the overlapping-run stamp), and `apply_row_selection` (the selection model — a device enters
  with its first ticked row and leaves with its last, and ticking one row must not tick the
  device's others).
- `date_to_epoch` / `epoch_to_date` — plain civil-date arithmetic rather than `js_sys::Date`, so
  they host-test, and `demo.rs` shares them instead of keeping a second copy.
- The pager (`page_count`/`clamp_page`/`page_bounds`/`pager_summary`/`prev_page`/`next_page`),
  the group-header count and the confirm-dialog gate
  (`needs_typed_confirmation`/`can_confirm_action`). The pager arithmetic once caused a "98% of
  groups unreachable" bug while sitting inline in `tables.rs`.
