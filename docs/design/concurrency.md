# Concurrency: blocking work and lock discipline

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/state.rs`, `src-tauri/src/commands/*.rs`, `src-tauri/src/auth.rs`,
`src-tauri/src/actions/audit.rs`.

## CPU-bound and blocking work goes on `spawn_blocking`, never on a tokio worker

A Tauri `async` command runs on the async runtime, so blocking there stalls unrelated IPC *and*
the job poller — `async` only buys you off the UI thread. That covers three kinds of work:

- **Seconds of CPU with no `.await` in it** — `commands::patches::run_query`'s `assemble_result`
  (the scope→join→sort→rollup).
- **Synchronous filesystem I/O** — `commands::export`'s workbook/report writes, `actions::audit`'s
  append.
- **Synchronous OS calls** — `commands::export`'s `blocking_save_file`, which parks until the
  operator picks a file; `auth::store_tokens`' keyring write, made while `refresh_lock` is held,
  i.e. exactly when every other `access_token()` caller is queued behind it.

**This list is illustrative, not exhaustive** — do not read it as an inventory of everywhere the
rule applies. An earlier version named "four places … and all of them are wrapped", and that
phrasing is precisely why four *more* blocking sites read as compliant to every reviewer until they
were measured: an audit write per device inside the dispatch `JoinSet`, a whole-fleet sort
permutation built inside a `std::sync::Mutex`, a keyring read under the `AuthState` write guard,
and an O(rows) deep copy of the result cache under the lock the paging commands take. Judge new
code against the rule, not against the examples.

## Hold `AppState`'s result mutex for a handle, not for the work

`with_current_result` is for a cheap projection; anything that needs the whole result for a while
(export, the HTML report) takes `current_result_handle`, which is an `Arc` bump.

## `AppState` locks are brief — never held across `.await`

`settings`/`last_result` are `std::sync::Mutex`. Take a `settings_snapshot()` (clone) before any
`.await`; don't hold a guard across an API call.
