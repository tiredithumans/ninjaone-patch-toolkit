---
paths:
  - "src-tauri/src/commands/**"
  - "src-tauri/src/lib.rs"
  - "web-rs/src/api.rs"
  - "web-rs/src/types.rs"
  - "web-rs/src/types/**"
  - "web-rs/tests/backend-ipc.json"
  - "src-tauri/src/fixtures.rs"
---

# IPC / Tauri command contract

**New Tauri command — 3 steps** (the `command-parity-check.sh` hook warns if you miss one):

1. `#[tauri::command] pub fn` (or `async fn` only if it awaits) in `src-tauri/src/commands/<domain>.rs`,
   `State<'_, AppState>` first, `Result<T, UiError>` out. A mutating handler calls
   `require_actions_enabled` — enforced by `every_mutating_command_checks_that_actions_are_enabled`.
2. Add `commands::<domain>::<name>` to `tauri::generate_handler![]` in `src-tauri/src/lib.rs`.
3. `ipc!(name(arg: T, …) -> Ret)` in `web-rs/src/api.rs` (+ mirror types in `web-rs/src/types.rs`).
   Arg keys and the command string are derived from the wrapper, so they cannot drift.

A command returning a new shape (or a new event) also adds one entry to
`fixtures::ipc_fixture_is_current` (`src-tauri/src/fixtures.rs`) and one to the decode table in
`web-rs/src/types/tests.rs`; the two must name the same entries.

- **Every shape the frontend decodes is pinned by `web-rs/tests/backend-ipc.json`**: generated
  by the backend from fixed inputs, decoded through each mirror by `types::tests`, which also
  fails on a mirror key the backend no longer sends (a rename hidden by `#[serde(default)]`).
  Every clock read is `FIXTURE_NOW`; every `Option`/`Vec` field needs a filled sample
  (`assert_every_field_is_exercised`). Regenerate with `just fixtures`; the diff is the wire
  change. Every `ipc!` wrapper must be in the decode table or in `NOT_DECODED` with its reason
  (`every_ipc_wrapper_is_decoded_or_listed` reads `api.rs`).
  → `docs/design/frontend.md#ipc-shapes-are-pinned-by-a-backend-generated-fixture`
- **Event listeners go through `api::subscribe`**, which logs an undecodable payload
  (`leptos::logging::warn!`) instead of dropping it silently.
- **Errors are `UiError { message }`; one the frontend branches on adds a `code`**
  (`UiError::coded(error::ERR_*)` + the `types.rs` mirror + a `coded` `ipc!` wrapper), never
  message matching. → `docs/design/frontend.md#tauri-commands`
- **IPC arg keys equal the handler's parameter names, camelCase.** Renaming a parameter is a
  wire-format change; update both sides. → `docs/design/frontend.md#ipc-arg-shape--keys-match-rust-fn-parameter-names-camelcase`
- **Compact aggregates (`failures`, `approvals`, `changes`, `worst_devices`, …) ride on both
  `QueryResult` and `QuerySummary`** (`approvals.stuck_devices` capped there). Add one in lockstep
  with `QuerySummary::from_result`, the `types.rs` mirror, the demo's `assemble`, and the
  regenerated IPC fixture. `QueryScope` and `instance` are the `QueryResult`-only exceptions.
  → `docs/design/query-cache.md#compact-aggregates-ride-in-the-summary-not-the-rows`
- **A new summary/result field touches six places** — `QueryResult` + `QuerySummary` +
  `from_result` + the `types.rs` mirror + `demo.rs` + the regenerated fixture (with a filled
  sample). A mirror field the summary does not carry fails `just web-test`, not a running app.