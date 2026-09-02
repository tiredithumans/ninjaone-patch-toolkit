---
name: review
description: Review PRs and commits for this repo — diff base → head, run verify gates on the right branches, check conventional-commits, flag IPC/secret/WASM/write-path footguns. Use when the user says "review", "approve this PR", or asks for a review of their changes.
argument-hint: "[PR number, commit sha, or branch name]"
---

# Review — inspect diffs, run gates, suggest fixes

Review work before it lands on `main`. The rules below are the ones a diff can violate
silently; each names where the rationale lives so you can check the intent, not just the text.

## 0. Find the work

- PR number → `gh pr view <num>` + `gh pr diff <num>`.
- Branch → `git fetch origin && git diff origin/main...<branch>`.
- Commit SHA → `git show <sha>`.
- No argument → `git status --short` + `git diff origin/main`.

## 1. Inspect the diff

**IPC / command chain** (`docs/design/frontend.md`)
- New command: handler in `src-tauri/src/commands/<domain>.rs`, entry in `generate_handler![]`,
  `ipc!` wrapper in `web-rs/src/api.rs`, types mirrored in `web-rs/src/types.rs`. A renamed
  parameter is a wire-format change on both sides.
- New summary/result field: `QueryResult` + `QuerySummary` + `from_result` + the types mirror +
  `demo.rs` + `serialized_shapes_carry_every_frontend_required_key`.

**Write path** (`docs/design/actions.md`) — the highest-risk surface
- Every mutating handler calls `require_actions_enabled` (the source-derived test must still pass).
- Every write POST goes through `post_action` / `post_json` with `ReplaySafety::ActOnce`; never
  `request_raw` with `Idempotent` for a write.
- A new `ActionKind` answers `is_mutating` / `supports_dry_run` correctly, is dispatched in
  `send_action`, sits under the `ACTION_GROUPS` heading that names its mechanism, and is mirrored
  in the frontend `ActionKind`.
- A new `ActionRequest` field is a compile error in `request_hash` (exhaustive destructure) — a
  diff that adds `..` there is wrong.
- Per-device targets, not a batch-wide list; a non-dry-run mutating action invalidates the
  current-patch cache.

**Query cache** (`docs/design/query-cache.md`)
- Rows are read via `with_current_result` / `current_result_handle`; no second copy of the rows,
  no direct access to `last_result`.
- Stores go through `store_last_result_if_current(token, …)` with a token claimed before the
  fetch; `TenantChanged` / `Poisoned` are errors, `Superseded` is not.
- Rollups take `&[&Patch]`; scoping borrows.

**Compliance semantics** (`docs/design/compliance.md`, `docs/design/severity.md`)
- A new rollup over the current feed uses `rollup_device`; a new facet goes in the right
  `QueryScope` tier; a new severity value walks the whole checklist.

**Concurrency / secrets / WASM**
- No `.await` while holding an `AppState` mutex; CPU-bound or blocking work on `spawn_blocking`.
- No token or secret reaches `settings.json` or a `tracing` event.
- No server crate (tokio, reqwest, keyring, rust_xlsxwriter) in `web-rs/Cargo.toml`.
- No `connect-src` CSP edit for a backend host.

**Frontend**
- Logic that could be asserted is in `web-rs/src/app/util/` with a test, not inline in a
  `#[component]`; a new dialog calls `modal::focus_trap()`.

**Docs**
- User-facing change → `CHANGELOG.md` `[Unreleased]`. New rule → AGENTS.md line + design note.
  Hook or skill change → `.claude/hooks/test.sh` still passes.

## 2. Run the gates

- `just verify` on the branch under review (required if any Rust source changed).
- `.claude/hooks/test.sh` if `.claude/` changed.
- `just audit` if a lockfile changed.

## 3. Check conventions

- Conventional Commits: `<type>[(scope)][!]: <description>`; scopes `desktop`, `web`, `api`,
  `auth`, `export`, `filter`, `settings`, `ci`, `docs`.

## 4. Produce output

```markdown
## Review Notes ✅ / ⚠️

### Changes reviewed
- `src-tauri/src/commands/patches.rs` — new status facet; handler + wrapper aligned.
- `web-rs/src/types.rs` — mirrors the new field.

### Issues
- [ ] ⚠️ `send_action` dispatches the new kind with `Idempotent` — must be `ActOnce`.
- [ ] `QuerySummary::from_result` does not clone the new aggregate.

### Gates
- ✅ `just verify` green on this branch.
```

## Failure handling

- `just verify` fails → report the gate output; do not approve.
- Parity gap, secret on disk/log, `Idempotent` write, or `..` in `request_hash` → blocking.
