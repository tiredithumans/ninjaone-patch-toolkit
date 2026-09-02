---
name: feature
description: Scaffold a new feature branch and command stub for the NinjaOne Patch Toolkit. Use when the user says "feature X", "add feature X", or asks to create a new command/wrapper.
argument-hint: "[feature description] — e.g., 'add feature export per-org compliance'"
---

# Feature — scaffold branches, commands, and IPC wrappers for new features

Branch → backend command → handler registration → `ipc!` wrapper → (optional) UI → verify.
Follow the patterns in AGENTS.md ("Common patterns"); the reasoning behind each rule is in
`docs/design/`.

## 0. Determine scope & naming

- What kind of change?
  - **New IPC command** → `src-tauri/src/commands/<domain>.rs` + `ipc!` in `web-rs/src/api.rs`.
  - **New NinjaOne API call** → a method on `NinjaApiClient` in `src-tauri/src/api/<domain>.rs`
    using `get_paginated` / `request_raw`; verify the endpoint against the spec first.
  - **New device action** → the 4-step pattern in AGENTS.md (POST via `post_action`/`post_json`,
    `ActionKind` variant with `is_mutating`/`supports_dry_run`, dispatch in `send_action`,
    button in `ACTION_GROUPS`) and the `web-rs/src/types.rs` mirror.
  - **New filter facet** → device facet in `PreparedFilter::device_allowed`, patch facet as a
    client-side `*_allowed()`; then the `QueryScope` tier it belongs to.
  - **New UI surface** → a component in the matching `web-rs/src/app/<module>.rs`; any logic
    worth asserting goes in `web-rs/src/app/util/` as a free function with a test.
- Conventional Commits scope: `desktop`, `web`, `api`, `auth`, `export`, `filter`, `settings`,
  `ci`, `docs`.
- Branch name: `<type>/<short-slug>` (e.g. `feat/org-compliance-export`).

## 1. Branch

```bash
git checkout -b <type>/<short-slug> origin/main
```

## 2. Backend command (`src-tauri/src/commands/<domain>.rs`)

```rust
use tauri::State;

use crate::error::UiError;
use crate::state::AppState;

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MyArgs {
    // arrives camelCase from the frontend
}

#[tauri::command]
pub async fn my_command(state: State<'_, AppState>, args: MyArgs) -> Result<MyResult, UiError> {
    // .map_err(UiError::from); take settings_snapshot() before any .await;
    // CPU-bound or blocking work goes on spawn_blocking.
}
```

- `async` only if the handler awaits something; a handler over in-process state is a plain
  `pub fn`.
- A mutating handler must call `require_actions_enabled` first —
  `every_mutating_command_checks_that_actions_are_enabled` fails otherwise.
- Anything that reads query rows goes through `state.with_current_result` /
  `current_result_handle`, never a second copy of the rows.

## 3. Register (`src-tauri/src/lib.rs`)

Add `commands::<domain>::my_command,` to `tauri::generate_handler![]`.

## 4. Frontend wrapper (`web-rs/src/api.rs`)

```rust
ipc!(my_command(args: MyArgs) -> MyResult);
// or, when the wrapper reads better under another name:
ipc!(short_name as "my_command", (args: MyArgs) -> MyResult);
```

The macro derives the command string and the camelCase arg keys from the wrapper's name and
parameters. Mirror `MyArgs` / `MyResult` in `web-rs/src/types.rs` (plain `String` for backend
`Arc<str>`), and if the result rides on `QuerySummary`, add the key to
`serialized_shapes_carry_every_frontend_required_key` and to `demo.rs`'s `assemble`.

## 5. UI (optional)

- Call `api::my_command(...)` from a signal-driven handler in `web-rs/src/app/state.rs` or the
  component module; render in `web-rs/src/app/<module>.rs`. CSS is global `web-rs/styles.css`.
- A new dialog calls `modal::focus_trap()` inside the closure that creates it.

## 6. Verify

`just verify` (both crates). If a hook or skill changed, also `.claude/hooks/test.sh`.
The `command-parity-check.sh` hook reports a missing registration or wrapper after each edit to
the chain.

## 7. Docs

- Add a line under `## [Unreleased]` in `CHANGELOG.md` for user-facing changes.
- New rule or invariant → one line in AGENTS.md pointing at the rationale in the matching
  `docs/design/<domain>.md`. New module → the AGENTS.md repo map.

## Output format

```
feature: created scaffold for <type>/<short-slug>

- ✅ Branch `<type>/<short-slug>` from origin/main
- ✅ Handler `src-tauri/src/commands/<domain>.rs::my_command` (+ generate_handler![])
- ✅ `ipc!(my_command(...))` in `web-rs/src/api.rs`, types mirrored in `web-rs/src/types.rs`
- ✅ `just verify` green

Next: implement the body and add the test that pins the new behavior.
```

## Failure handling

- Handler already registered → skip, say so.
- Parity hook warns → add the missing half before verifying.
- New dependency → it must not enter `web-rs` if it is a server crate; check `just deny` policy.
