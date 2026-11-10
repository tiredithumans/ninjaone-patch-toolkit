---
paths:
  - "src-tauri/src/**/*.rs"
  - "src-tauri/build.rs"
  - "justfile"
---

# Backend core (concurrency & locks)

- **CPU-bound and blocking work goes on `spawn_blocking`** — `assemble_result`, workbook/report
  writes, the audit append, the save dialog, keyring I/O. Judge new code against the rule, not
  against that list. → `docs/design/concurrency.md`
- **`AppState` locks are brief and never held across `.await`.** Take `settings_snapshot()`
  first; hold the result mutex for a handle (`current_result_handle`), not for the work. Settings
  writers go through `AppState::write_settings` (the one owner of the writer lock: re-read the file
  over the snapshot (`Settings::rebased_on_disk`, connection fields kept), edit,
  save on a blocking thread, publish after the disk write; keep its guard across any follow-up
  effects); only an instance/client-id change clears caches on save.
  → `docs/design/concurrency.md`
- **A file that is rewritten goes through `paths::write_atomic`/`write_atomic_with`** (pid +
  counter temp, sync, rename; `owner_only` for anything naming devices) — never a hand-rolled
  temp-and-rename. Tests: `paths::tests::*atomic*`.
  → `docs/design/concurrency.md#every-rewritten-file-goes-through-pathswrite_atomic`
- **Windows links an 8 MiB main-thread stack** (`src-tauri/build.rs`, MSVC only,
  `rustc-link-arg-bins`; never `.cargo/config.toml` or `RUSTFLAGS`).
  → `docs/design/concurrency.md`