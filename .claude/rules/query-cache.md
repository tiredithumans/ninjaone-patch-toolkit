---
paths:
  - "src-tauri/src/state/**"
  - "src-tauri/src/commands/patches.rs"
  - "src-tauri/src/rows/groups.rs"
---

# Query cache & result store

- **`AppState.last_result` is the single source of truth for paging, export and the HTML
  report.** Write via `store_last_result_if_current(token, result)`, read via
  `with_current_result` / `current_result_handle` (memos via `sort_memo` / `group_memo` +
  `store_*_memo`); never touch the slot directly. → `docs/design/query-cache.md`
- **Claim the `QueryToken` (`begin_query`) before any fetch and redeem it at the store.** A
  superseded or tenant-drifted result is dropped. `StoreOutcome::Superseded` still returns the
  summary; `TenantChanged`/`Poisoned` are errors (`commands::patches::summary_for`).
  → `docs/design/query-cache.md#the-write-is-generation--and-tenant-gated`
- **Tenant switch, sign-out, sign-in and re-authorize all call `clear_session()`** on the
  frontend and `clear_session_state` on the backend.
  → `docs/design/query-cache.md#a-tenant-switch-a-sign-out-a-sign-in-and-a-re-authorization-all-clear-the-frontend`
- **Paging/grouping/sorting commands (and `device_detail`) return empty on a cache miss, never
  an error.** Sort/group memos live in `CachedResult` (built on `spawn_blocking`, stored only if
  `Arc::ptr_eq` holds); the cached rows are never reordered. Group headers carry no members;
  never regroup `page_rows` client-side. `demo.rs` mirrors `group_key`, pinned by
  `web-rs/tests/backend-grouping.json`.
  → `docs/design/query-cache.md#paging-commands-return-empty-on-a-miss-never-an-error`
- **Compact aggregates (`failures`, `approvals`, `changes`, `worst_devices`, …) ride on both
  `QueryResult` and `QuerySummary`.** Add one in lockstep with `QuerySummary::from_result`, the
  `types.rs` mirror, the demo's `assemble`, and a filled sample in the regenerated IPC fixture
  (`web-rs/tests/backend-ipc.json`, see `ipc-contract.md`).
  → `docs/design/query-cache.md#compact-aggregates-ride-in-the-summary-not-the-rows`
- **Every TTL'd cache slot is a `TenantCache<T>`** — it owns the tenant stamp, TTL,
  single-flight gate, and the epoch sampled before the fetch and re-checked at the store. Never
  open-code that protocol for a new slot; `last_result` is the one exception and is
  generation-gated instead. → `docs/design/query-cache.md#one-cache-protocol-one-type`
- **Devices and current patches are fetched whole-fleet and scoped client-side** via
  `PreparedFilter::device_allowed`; the OS and third-party families are separate cache slots and
  only the requested family is fetched. Stores are epoch-gated and fetches are single-flight per
  family. `force_refresh` is floored backend-side by `FORCE_MIN_INTERVAL`.
  → `docs/design/query-cache.md#whole-fleet-prefetch--client-side-scoping`
- **Scoping borrows, never clones.** Rollups take `&[&Patch]`; don't reintroduce an owned
  `Vec<Patch>`. → `docs/design/query-cache.md#scoping-borrows-never-clones`
