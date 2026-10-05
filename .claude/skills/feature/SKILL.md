---
name: feature
description: Scaffold a new feature branch and command stub for the NinjaOne Patch Toolkit. Use when the user says "feature X", "add feature X", or asks to create a new command/wrapper.
argument-hint: "[feature description] — e.g., 'add feature export per-org compliance'"
---

# Feature — scaffold branches, commands, and IPC wrappers for new features

Branch → determine the change kind → read the matching `.claude/rules/*.md` → implement along
the chain → verify. The full per-domain contracts live in `.claude/rules/`; the reasoning is in
`docs/design/`. (Reading a rule file here also pre-loads it for the edits that follow.)

## 0. Determine scope & naming

Read the matching rule file **before** writing anything:

- **New IPC command** → `.claude/rules/ipc-contract.md` — the 3-step chain (handler →
  `generate_handler![]` → `ipc!` wrapper + type mirror).
- **New NinjaOne API call** → `.claude/rules/api-client.md`; verify the endpoint against
  `docs/api/ninjaone-surface.md` first, never from memory.
- **New device action** → `.claude/rules/actions-write.md` — the 4-step chain; guardrails go in
  `actions::plan`, not in a dialog.
- **New filter facet** → `.claude/rules/filter.md` — device facet vs client-side patch facet.
- **New UI surface** → `.claude/rules/frontend.md`; any logic worth asserting goes in
  `web-rs/src/app/util/` as a free function with a test.
- Conventional Commits scope: `desktop`, `web`, `api`, `auth`, `export`, `filter`, `settings`,
  `ci`, `docs`.
- Branch name: `<type>/<short-slug>` (e.g. `feat/org-compliance-export`).

## 1. Branch

```bash
git checkout -b <type>/<short-slug> origin/main
```

## 2. Implement along the chain

- **Backend command:** `src-tauri/src/commands/<domain>.rs` — `State<'_, AppState>` first,
  `Result<T, UiError>` out, `async` only if it awaits; a mutating handler starts with
  `require_actions_enabled`; anything reading query rows goes through `with_current_result` /
  `current_result_handle`, never a second copy of the rows. CPU-bound or blocking work on
  `spawn_blocking`; take `settings_snapshot()` before any `.await`.
- **Register:** add `commands::<domain>::<name>,` to `tauri::generate_handler![]` in
  `src-tauri/src/lib.rs`.
- **Frontend wrapper:** `ipc!(name(args: MyArgs) -> MyResult)` in `web-rs/src/api.rs` (arg keys
  and command string derive from the wrapper — they cannot drift). Mirror `MyArgs`/`MyResult` in
  `web-rs/src/types.rs` (plain `String` for backend `Arc<str>`); if the result rides on
  `QuerySummary`, add the key to `serialized_shapes_carry_every_frontend_required_key` and to
  `demo.rs`'s `assemble`.
- **UI (optional):** call `api::my_command(...)` from a signal-driven handler in
  `web-rs/src/app/state/<concern>.rs` or the component module; render in
  `web-rs/src/app/<module>.rs`; CSS is global `web-rs/styles.css`; a new dialog calls
  `modal::focus_trap()` inside the closure that creates it.

## 3. Verify

`just verify` (both crates). If a hook or skill changed, also `.claude/hooks/test.sh`.
The `command-parity-check.sh` hook reports a missing registration or wrapper after each edit to
the chain.

## 4. Docs

- Add a line under `## [Unreleased]` in `CHANGELOG.md` for user-facing changes.
- New rule or invariant → one line in the matching `.claude/rules/*.md` pointing at the
  rationale in the matching `docs/design/<domain>.md`; touch the AGENTS.md index only when a
  cross-domain flow changed. New module → `docs/architecture.md`.

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