# Query result cache and whole-fleet prefetch

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/state.rs` (`AppState`), `src-tauri/src/commands/patches.rs`
(`query_patches`, `run_query`, `assemble_result`), `src-tauri/src/rows/`.

## The cache is the single source of truth for export, the report, and paging

`query_patches` caches the full `QueryResult` in `AppState.last_result` (a `Mutex`) on success
and returns only a lightweight `QuerySummary` (first page of rows + `rows_total` + the
reboot-device subset + compliance + the compact dashboard/failure aggregates) over IPC — a 10k+
row fleet is never serialized wholesale into the WASM webview. The detail table pages the rest on
demand via `get_patch_rows(offset, limit)`, which slices the same cache; `export_patches_xlsx`
**and** `export_report_html` read it too. So the cache is the single source of truth for export,
the HTML report, **and** row paging: any of them with no prior successful query = empty. Don't add
a second source of truth for the rows.

## Tenant-keyed, method-gated access

`last_result` is **private** and stamped with the tenant
(`Mutex<Option<(TenantKey, QueryResult)>>`, `TenantKey` = instance URL + client id). Never touch
it directly — write via `state.store_last_result_if_current(token, result)` and read via
`state.with_current_result(|r| …)`, which compare the stamp at read time, so a result from a
different tenant reads as a miss. The whole-fleet input caches (devices/current/lookups) carry the
same stamp. This makes the `clear_lookups_cache` / `clear_last_result` calls (sign-out, instance
change) belt-and-suspenders for correctness — a forgotten one can't leak a prior tenant's rows;
they remain only to reclaim memory promptly and to wipe rows on an explicit same-tenant sign-out.

## The write is generation- and tenant-gated

`query_patches` claims a `QueryToken` via `state.begin_query()` **before** any fetch and redeems
it at the store. Two things ride on that ordering. Overlapping queries (an auto-refresh tick
during a manual Run) are ordered by *start*, not by completion, so the run the frontend renders is
the run whose rows are cached — otherwise the visible table and the rows behind paging/export come
from different queries. And the tenant stamp is taken from the token, i.e. the tenant the query
was *fetched* under; a whole-fleet fetch runs for minutes, so stamping at write time could file
the old tenant's rows under the new one — the one way the tenant check can be *wrong* rather than
merely miss. A superseded or tenant-drifted result is dropped, not stored.

## The three drop reasons are distinct, and the caller must treat them differently

`store_last_result_if_current` returns a `StoreOutcome`, not a bool. `Superseded` is invisible to
the operator and the frontend already discards the response itself (`run_query` compares
`query_seq` after the await and drops a superseded one, while still clearing its own busy flag),
so the summary is still returned. `TenantChanged` and `Poisoned` are **errors**:
`commands::patches::summary_for` refuses to hand back a renderable summary, because the frontend
has no equivalent guard — `query_seq` counts runs the frontend *starts*, and switching instance
never bumps it, so returning the summary painted the previous tenant's rows over the new tenant's
empty cache while paging and export read the miss. The rule: return a summary only when the rows
behind it are readable.

## A tenant switch, a sign-out, a sign-in and a re-authorization all clear the frontend

`save_settings` reports `tenant_changed` on its `SettingsView` (both halves of the tenant key —
instance *and* client id), and `apply_settings_view` calls `clear_session()` and resets the
org/location/role scope ids (they belong to the previous tenant's lookups). `clear_session` drops
everything `commands::auth::clear_session_state` drops backend-side — the result, the job list and
any pending confirmation — and the sign-in, sign-out and Re-authorize handlers call it too.
Without it the previous session's rows stayed rendered against a cache that had already been
dropped: Next page came back blank under "Rows 101–200 of N" and Export said "Run a query before
exporting" beside a visible table — the same divergence one layer up.

## Paging commands return empty on a miss, never an error

The three paging commands all return empty on a cache miss. A miss is a normal transient (tenant
switch, sign-out, superseded query); the frontend already renders its own empty state from the
absent result.

`get_patch_rows` also takes an optional `sort` (`rows::RowSort`), applied through
`AppState::with_sorted_result` — the cached rows themselves are never reordered; their canonical
severity/org/device order feeds the export and the summary's inline first page.

## Both derived views are memoized inside the cache slot

`CachedResult` carries `groups: Option<(GroupBy, …)>` **and** `sorted: Option<(RowSort,
Arc<Vec<u32>>)>`. `rows::sort_order` builds an index permutation; `rows::page_rows` slices it (or
the cache order when `None`, so an unsorted fleet never materializes an identity permutation).
Paging a sorted view once re-sorted every cached row on **every page request**, under the lock the
export also takes. Both memos live *inside* the slot, so replacing or clearing the result drops
them in the same operation and there is no second staleness protocol to get wrong.

## Grouping is backend-side too

The Patches tab's *By device* / *By patch* modes go through `get_patch_groups` (headers + total,
`rows::group_page`) and `get_patch_group_members` (one group's rows, `rows::group_member_page`).
The frontend only ever holds one page, so it cannot group a fleet it has never seen — never
regroup `page_rows` client-side. Group headers carry **no** members: a by-patch group can span the
whole fleet, so members load on expand, capped at `GROUP_MEMBER_LIMIT`. `rows::group_key` is the
identity the frontend echoes back, so no per-request state is kept backend-side and a stale key
matches nothing. `demo.rs` mirrors `group_key`/`build_groups` by hand for the browser demo — keep
the two in step.

## Compact aggregates ride in the summary, not the rows

Fleet-wide distributions the frontend charts/failure tab need — `failures` (FAILED-install
rollup, `build_failures`), `severity_by_org` (`build_severity_by_org`), `age_buckets`
(`build_age_buckets`) — are computed backend-side in `rows/` and carried on **both** `QueryResult`
(cached; the HTML report reads it) and `QuerySummary` (IPC; the dashboard reads it). They're
bounded (one entry per failing patch / per org / 5 buckets), so they ship whole rather than paged.
Add such a field in lockstep: `QueryResult` + `QuerySummary` + clone in `QuerySummary::from_result`
+ the `web-rs/src/types.rs` mirror + the demo's `assemble`, and assert its key in
`serialized_shapes_carry_every_frontend_required_key`. Keep the backend `QuerySummary` ⇄ frontend
`QueryResult` (`web-rs/src/types.rs`) shapes in sync.

The one documented exception is `QueryScope`, which lives on `QueryResult` only — see
[compliance.md](./compliance.md#both-exports-state-the-facets-from-rowsqueryscope).

## Whole-fleet prefetch + client-side scoping

The device inventory and current patches (OS + 3rd-party) are fetched **whole-fleet** (no `df`)
and cached in `AppState` (`fleet_devices_cache`, `DEVICE_TTL` ~15 min since devices change rarely;
the current patches in `fleet_current_os` / `fleet_current_sw`, refreshed on `force_refresh` or
past `CURRENT_PATCHES_TTL`). `run_query` then scopes them to the selected identity facets
(org/location/role/class) **client-side** via `FilterParams::device_allowed` — so changing
org/location/role/type/severity re-filters the cache with **no** round trip. This is why
`query_patches` takes the cached devices/current as *futures* (concurrent cold fetch) and why there
is no separate per-query device-filter fetch.

### The two current-patch families are cached separately, and only the requested one is fetched

`fleet_current_patches` takes `include_os` / `include_sw` straight from the query's `PatchType`; a
family that wasn't asked for is neither fetched nor returned (`run_query` discards it anyway).
They are separate slots because the families are wildly asymmetric — a whole-fleet third-party
feed runs to six figures and is usually the largest fetch in the query, so fetching it for an `Os`
query cost ~80 serial cursor pages of data that was then dropped, *and* made it the critical path
(an OS-only query took about as long as an ALL query). Widening `Os` → `All` still reuses whatever
is warm. Don't merge them back into one pair.

### The stores are epoch-gated, and the fetches are single-flight

A whole-fleet fetch runs for minutes, so a mutating action's `invalidate_current_patches()`
routinely lands while one is in flight. An unconditional store lets the in-flight fetch write its
pre-action rows straight back with `CURRENT_PATCHES_TTL` restarted on them — the tenant stamp
cannot catch this, it is the same tenant. `devices_epoch` / `current_epoch` are sampled before the
fetch and re-read **under the slot lock** at the store; the invalidators bump the epoch *before*
clearing the slot, so the two orderings both lose the write. Separately, each cache has a
`tokio::Mutex` fetch gate (mirroring `AuthState::refresh_lock`) with a re-check after acquiring:
queries overlap by design, and on a cold cache both callers would otherwise page the entire
inventory / third-party feed independently. Per family, so an OS-only query never waits on an
in-flight third-party fetch.

### `force_refresh`

`force_refresh` (camelCase `forceRefresh`, the auto-refresh tick / manual ↻) trades
`CURRENT_PATCHES_TTL` for `FORCE_MIN_INTERVAL` to pull fresh patch state mid-patching; a normal
Run query leaves it false. The floor is enforced **backend-side on purpose**: the frontend cadence
is a hint, and unbounded `force` makes the whole cache decorative on the one path that runs
unattended for hours. The frontend also skips a tick while `document.hidden`
(`api::document_hidden`).

### Install history is not prefetched

Install history is fetched fresh per query, scoped server-side by `patch_filter` +
status-pushed-down (too large to cache). The summary carries `data_fetched_at` (when the patch
data was last fetched, distinct from `generated_at`) for the UI's "patch data as of …" label. The
whole-fleet caches are tenant-scoped, so `clear_lookups_cache` drops them too.

### Scoping borrows, never clones

`run_query` filters the cached `Arc`s into `Vec<&Patch>`, and the rollups (`pending_counts`,
`build_compliance`, `build_compliance_by_os`, `build_severity_by_org`, `build_age_buckets`) plus
`PatchSource.patches` all take `&[&Patch]`. A whole-fleet third-party feed runs to six figures and
each `Patch` owns **seven** `Option<String>`s, so cloning the scoped subset — and again into
`all_current` — costs millions of allocations per query for data the cache already owns and
outlives. Keep new rollups on `&[&Patch]`; don't reintroduce an owned `Vec<Patch>` to make a
signature more convenient.
