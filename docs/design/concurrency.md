# Concurrency: blocking work and lock discipline

Contract lines: [AGENTS.md → Domain rules](../../AGENTS.md#domain-rules--read-the-file-before-you-change-the-domain).
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

One deliberate exception: `window_state` writes its few-dozen-byte file synchronously on the
event-loop thread when the window **closes**, because a task spawned at that moment may never run;
its move/resize saves are debounced onto a blocking thread like everything else.

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

The one deliberate exception is the settings *writer* lock, a `tokio` mutex that only
`AppState::write_settings` takes: it snapshots, applies the edit, saves on a blocking thread and
publishes in memory only once the file is written, so two saves cannot interleave and lose one
another's change, and a failed write never leaves memory and `settings.json` disagreeing. Readers
never take it. `write_settings` hands its guard back so a writer with follow-up effects (a tenant
switch re-pointing auth and clearing caches) keeps later writers out until those have landed too.
The lock and `replace_settings` are private precisely so no second writer can hand-roll the
sequence without them.

The edit starts from `settings.json` as it is on disk, read under the writer lock
(`Settings::rebased_on_disk`), not from the in-memory snapshot. The lock serializes writers inside
one process only; a second app instance shares the file, and editing the snapshot meant instance A's
next preset save wrote back everything A had loaded at launch — reverting instance B's change in
between, actions being switched off included. The connection fields (instance, client id, callback
port) are kept from the snapshot: they are bound to this process's signed-in session and caches,
which only `save_settings`' effects re-point. An unreadable or unparseable file falls back to the
snapshot. The other instance's in-memory copy is still stale until its own next write — a reader
does not re-read; `tauri-plugin-single-instance` would remove the second instance altogether, at
the cost of a dependency.

## Every rewritten file goes through `paths::write_atomic`

Settings, window state, the exploit data, the run snapshots, the first-seen ledger, the run-history
trim and every export replace their file through `paths::write_atomic` (or `write_atomic_with`,
which hands the caller the `File` to stream into): a hidden temp beside the file, synced, renamed
over it, removed on failure, 0600 when `owner_only`. They used to be six hand-rolled copies with
three behaviours — a fixed `.tmp` name in three, so two app instances writing one file could
interleave into one temp and rename the mixture; no `sync_all` in two, so a power cut after the
rename could leave an empty file. The temp is `.name.tmp-<pid>-<n>`: the pid separates processes,
the counter separates threads, and the leading dot keeps a directory scan by extension from
reading one as data.

## The Windows main thread has a 1 MiB stack

Windows MSVC gives the main thread 1 MiB; macOS and Linux give 8 MiB. v0.15.0's optimized build
overflowed it on every Run query (`STATUS_STACK_OVERFLOW`, `thread 'main' has overflowed its
stack`) while `just dev` (debug) did not, so the overflow only exists in an optimized Windows build.
`src-tauri/.cargo/config.toml` links an 8 MiB stack for `x86_64-pc-windows-msvc`, which fixed it.

The query itself is not the cause: assembling 548,000 software patches, diffing and serializing the
summary passes on a 1 MiB stack off the main thread. The frames that use main-thread stack at the end
of a query were never identified, so this raises the limit rather than removing the cause. Do not
set it with `RUSTFLAGS`: that also reaches the Trunk WASM build and build scripts and fails them.

