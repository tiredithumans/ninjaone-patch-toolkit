---
paths:
  - "src-tauri/src/**/*.rs"
  - "src-tauri/build.rs"
  - "src-tauri/.cargo/config.toml"
  - "justfile"
---

# Backend core (concurrency & locks)

- **CPU-bound and blocking work goes on `spawn_blocking`** — `assemble_result`, workbook/report
  writes, the audit append, the save dialog, keyring I/O. Judge new code against the rule, not
  against that list. → `docs/design/concurrency.md`
- **`AppState` locks are brief and never held across `.await`.** Take `settings_snapshot()`
  first; hold the result mutex for a handle (`current_result_handle`), not for the work. Settings
  writers go through `AppState::write_settings` (the one owner of the writer lock: snapshot, edit,
  save on a blocking thread, publish after the disk write; keep its guard across any follow-up
  effects); only an instance/client-id change clears caches on save.
  → `docs/design/concurrency.md`
- **Windows links an 8 MiB main-thread stack** (`src-tauri/.cargo/config.toml`, never
  `RUSTFLAGS`). → `docs/design/concurrency.md`