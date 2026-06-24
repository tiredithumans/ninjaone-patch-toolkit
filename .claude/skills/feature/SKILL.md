---
name: feature
description: Scaffold a new feature branch and command stub for the NinjaOne Patch Toolkit. Use when the user says "feature X", "add feature X", or asks to create a new command/wrapper.
argument-hint: "[feature description] — e.g., 'add feature export per-org compliance'"
---

# Feature — scaffold branches, commands, and IPC wrappers for new features

Create a complete feature scaffolding: branch → backend command + handler → frontend IPC wrapper →
wiring. Follow the repo's conventions and run verification at each step.

## 0. Determine scope & naming

- From the argument (or ask): what is the feature?
  - New IPC command → extend or add a file in `src-tauri/src/commands/<domain>.rs` + a wrapper in
    `web-rs/src/api.rs`.
  - New NinjaOne API call → add to `src-tauri/src/api/` (`devices.rs` / `patches.rs` / `lookups.rs`).
  - New UI surface → a component/view in `web-rs/src/app.rs` (single-crate Leptos app).
- Determine the Conventional Commits scope: `desktop`, `web`, `api`, `auth`, `export`, `filter`.
- Suggest branch name: `<type>/<short-slug>` (e.g., `feat/org-compliance-export`).

## 1. Branch

```bash
git checkout -b <type>/<short-slug> origin/main
```
If on `main`, this creates a clean branch. If already on one, suggest rebasing.

## 2. Scaffolding — backend command (`src-tauri/src/commands/<domain>.rs`)

Add or extend a handler under the matching module (`auth` / `lookups` / `patches` / `export` /
`settings`). Use the repo pattern:
```rust
use tauri::State;

use crate::error::UiError;
use crate::state::AppState;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MyArgs {
    // frontend args arrive camelCase
}

#[tauri::command]
pub async fn my_command(
    state: State<'_, AppState>,
    args: MyArgs,
) -> Result<MyResult, UiError> {
    // ... implementation; map errors with .map_err(UiError::from)
}
```

- `#[tauri::command]`, `State<'_, AppState>` first, `Result<T, UiError>` out (`UiError` serializes to
  `{ message }` for the frontend toast).
- Domain types live in `src-tauri/src/model.rs`; row/rollup builders in `rows.rs`; the NinjaOne `df`
  device-filter builder in `filter.rs`.
- If the command needs the API client: `state.api.clone()` (see `commands/patches.rs`).

## 3. Register the handler (`src-tauri/src/lib.rs`)

Add to `tauri::generate_handler![]`:
```rust
.invoke_handler(tauri::generate_handler![
    // ... existing handlers
    commands::<domain>::my_command,
])
```

## 4. Create the frontend wrapper (`web-rs/src/api.rs`)

Add a typed `async fn` that calls `invoke(...)`. The arg object's keys must match the Rust fn
parameter names (camelCase). If the param is a single struct named `args`, wrap it:
```rust
pub async fn my_command(args: MyArgs) -> Result<MyResult, String> {
    #[derive(Serialize)]
    struct Wrap { args: MyArgs }
    invoke("my_command", args_of(&Wrap { args })).await
}
```
Mirror the request/response types in `web-rs/src/types.rs` (kept in sync with `src-tauri/src/model.rs`
and the command's arg/result structs).

## 5. If a UI surface — wire it into `web-rs/src/app.rs`

- Add the component/handler and call the new `api::my_command(...)`.
- CSS is plain global `web-rs/styles.css` (BEM-ish names).

## 6. Verify scaffolding passes

Run `just verify` (fmt-check → clippy → test → web-check → web-clippy) and report issues. A
frontend-only change still needs `just web-check` + `just web-clippy`.

## 7. Output format

```
feature: created scaffold for <type>/<short-slug>

## Changes
- ✅ Branch created: `<type>/<short-slug>` from `origin/main`
- ✅ Backend handler: `src-tauri/src/commands/<domain>.rs`
- ✅ Handler registered in `src-tauri/src/lib.rs` (`generate_handler![]`)
- ✅ Frontend wrapper: `web-rs/src/api.rs` (calls `invoke("my_command", …)`)
- ✅ Shared types mirrored in `web-rs/src/types.rs`

## Next steps
Write the implementation in `src-tauri/src/commands/<domain>.rs` and fill in the types.
```

## Failure handling

- If `generate_handler![]` already has the handler → warn (skip duplicate).
- If `command-parity-check.sh` warns about a missing invoke wrapper → add one to `web-rs/src/api.rs`.
- New dependency → check the crate's `Cargo.lock` for conflicts before adding; prefer existing deps.
