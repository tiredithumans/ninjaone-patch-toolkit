---
name: review
description: Review PRs and commits for this repo — diff base → head, run verify gates on the right branches, check conventional-commits, flag IPC/secret/WASM footguns. Use when the user says "review", "approve this PR", or asks for a review of their changes.
argument-hint: "[PR number, commit sha, or branch name]"
---

# Review — inspect diffs, run gates, suggest fixes

Review work in progress before it lands on `main`. Use the repo's exact verification pipeline and
conventions. If arguments were passed, treat them as guidance (PR number, commit SHA, or branch).

## 0. Find the work

- **Argument given:**
  - PR number → `gh pr view <num>` for diffs + status.
  - Branch name → `git fetch origin && git diff origin/main...<branch>`.
  - Commit SHA → `git show <sha>` + `gh pr search --head <sha>`.
- **No argument:** diff working tree → `main` via `git status --short` + `git diff origin/main`.

## 1. Inspect the diffs

- **Backend (`src-tauri/`):** new commands must follow the 3-step pattern:
  1. `#[tauri::command] async fn` in `src-tauri/src/commands/<domain>.rs` returning `Result<T, UiError>`.
  2. Added to `tauri::generate_handler![]` in `src-tauri/src/lib.rs`.
  3. Typed wrapper in `web-rs/src/api.rs` calling `invoke("<command>", …)` (the
     `command-parity-check.sh` hook catches gaps; mention which one).
- **camelCase ↔ snake_case:** frontend arg structs use `#[serde(rename_all = "camelCase")]`; the
  invoke arg object's keys must match the Rust fn parameter names. Confirm `web-rs/src/types.rs`
  mirrors `src-tauri/src/model.rs` / command structs.
- **Secrets discipline:** the client secret and refresh token belong only in the OS keyring
  (`auth.rs`); nothing sensitive may be written to `settings.json`. Flag any token/secret that
  reaches disk or a `tracing` log.
- **`query_patches` → export coupling:** `query_patches` caches its `QueryResult` in
  `state.last_result`; `export_patches_xlsx` reads that cache. A change to one must not desync the
  other (export with no prior query = nothing to write).
- **WASM gating:** any server-only dep (tokio, reqwest, keyring) used from `web-rs`? The frontend is
  a separate `wasm32-unknown-unknown` crate — those belong in `src-tauri` only.
- **CSP:** new NinjaOne hosts are reached by the **backend** reqwest client, not the webview, so
  they need **no** `connect-src` change in `tauri.conf.json`. Flag a CSP edit added "just in case".

## 2. Run the gates

- **Full:** `just verify` (fmt-check → clippy → test → web-check → web-clippy) on the current branch
  (or the PR branch). Required if any Rust/WASM source changed.
- **Quick variant (large PRs):** `just clippy` + `just test` if the frontend is known stable.
- **Dependency audit** (if new deps): `just audit` (scans both lockfiles).

## 3. Check conventions

- Commits follow Conventional Commits (`<type>[(scope)][!]: <description>`).
- Scopes match: `desktop`, `web`, `api`, `auth`, `export`, `filter`, `settings`, `ci`, `docs`.

## 4. Produce output

Review result in PR-comment form when reviewing via `gh`:
```markdown
## Review Notes ✅ / ⚠️

### Changes reviewed:
- `src-tauri/src/commands/patches.rs` — new status filter, handler + wrapper aligned.
- `web-rs/src/api.rs` — invoke wrapper matches backend (camelCase args).
- `web-rs/src/types.rs` — mirrors the new model fields.

### Issues:
- [ ] ⚠️ `export_patches_xlsx` reads `state.last_result` but the new field isn't populated by `query_patches`.
- [ ] Secret value logged via `tracing::debug!` in `auth.rs` — redact.

### Gates:
- ✅ fmt-check, clippy (0 warnings), test, web-check, web-clippy — passed on this PR.
```

## Failure handling

- `just verify` fails → report the failing gate's output; do not approve.
- Unmatched command/wrapper pair → flag it (check `command-parity-check.sh`).
- Secret reaching disk or logs → high-priority warning.
