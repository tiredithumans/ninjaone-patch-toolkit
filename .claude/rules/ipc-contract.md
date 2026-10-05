---
paths:
  - "src-tauri/src/commands/**"
  - "src-tauri/src/lib.rs"
  - "web-rs/src/api.rs"
  - "web-rs/src/types.rs"
---

# IPC / Tauri command contract

**New Tauri command — 3 steps** (the `command-parity-check.sh` hook warns if you miss one):

1. `#[tauri::command] pub fn` (or `async fn` only if it awaits) in `src-tauri/src/commands/<domain>.rs`,
   `State<'_, AppState>` first, `Result<T, UiError>` out. A mutating handler calls
   `require_actions_enabled` — enforced by `every_mutating_command_checks_that_actions_are_enabled`.
2. Add `commands::<domain>::<name>` to `tauri::generate_handler![]` in `src-tauri/src/lib.rs`.
3. `ipc!(name(arg: T, …) -> Ret)` in `web-rs/src/api.rs` (+ mirror types in `web-rs/src/types.rs`).
   Arg keys and the command string are derived from the wrapper, so they cannot drift.

- **Errors are `UiError { message }`; one the frontend branches on adds a `code`**
  (`UiError::coded(error::ERR_*)` + the `types.rs` mirror + a `coded` `ipc!` wrapper), never
  message matching. → `docs/design/frontend.md#tauri-commands`
- **IPC arg keys equal the handler's parameter names, camelCase.** Renaming a parameter is a
  wire-format change; update both sides. → `docs/design/frontend.md#ipc-arg-shape--keys-match-rust-fn-parameter-names-camelcase`
- **Compact aggregates (`failures`, `approvals`, `changes`, `worst_devices`, …) ride on both
  `QueryResult` and `QuerySummary`** (`approvals.stuck_devices` capped there). Add one in lockstep
  with `QuerySummary::from_result`, the `types.rs` mirror, the demo's `assemble`, and
  `serialized_shapes_carry_every_frontend_required_key`. `QueryScope` and `instance` are the
  `QueryResult`-only exceptions. → `docs/design/query-cache.md#compact-aggregates-ride-in-the-summary-not-the-rows`
- **A new summary/result field touches six places** — `QueryResult` + `QuerySummary` +
  `from_result` + the `types.rs` mirror + `demo.rs` + the shape test. A diff that adds a field to
  one shape but not the mirror deserializes as a "decode <cmd>" error toast.