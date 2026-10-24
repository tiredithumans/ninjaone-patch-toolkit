# Query result cache and whole-fleet prefetch

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/state.rs` (`AppState`), `src-tauri/src/state/cache.rs` (`TenantCache`),
`src-tauri/src/commands/patches.rs` (`query_patches`, `run_query`, `assemble_result`),
`src-tauri/src/rows/`.

## One cache protocol, one type

`TenantCache<T>` (`state/cache.rs`) owns the whole protocol every TTL'd slot needs:

1. sample the epoch on *entry*, before the first probe;
2. probe the slot — tenant match **and** within the TTL — and return a hit;
3. take the single-flight gate, so the loser of a race waits instead of paging a
   six-figure feed a second time;
4. re-probe under the gate, which is what turns that wait into a cache hit;
5. store only if the epoch is unchanged when the slot lock is retaken;
6. a degraded fetch (an optional sub-feed failed and fell back to empty) stores under a
   capped TTL, `FORCE_MIN_INTERVAL`, never the slot's full TTL.

Step 5 is the subtle one. Without it, a fetch already in flight when an invalidation
lands writes its pre-invalidation rows straight back and restarts the TTL on exactly
the data the caller wanted gone — and the tenant stamp cannot cover it, because a
sign-out or a post-action invalidation is the *same* tenant.

Step 1 used to come after the gate, while the caller had read its tenant key before
calling. A tenant switch that landed while a caller was parked behind another fetch
went unseen, and the store went ahead under the departed tenant's key. Sampled at
entry, any invalidation during the wait declines the store. The cost is that a caller
which waited across a same-tenant invalidation does not cache its fresh fetch; the next
caller does.

Step 6 exists because the fallbacks were cached exactly like a clean fetch: a `/groups`
or `/locations` failure left the Group filter or the location names blank for the full
five-minute `LOOKUP_TTL`, a `/queries/device-health` failure hid every reboot reason for
the fifteen-minute `DEVICE_TTL`, and a forced refresh cleared neither. The fetch closure
returns `Fetched { value, degraded }` and the entry carries the cap, so `peek` serves it
for `min(ttl, cap)`. The cap is not zero ("don't store"): a token that permanently
cannot list `/groups` would then refetch the organizations and roles on every query.
`lookups()` is degraded when locations or groups fell back, `fleet_devices()` when the
health feed failed; the current-patch families have no optional part and always store
clean (`Fetched::complete`).

A value displaced from a slot (by `invalidate` or by a store) is dropped after the slot
lock is released, not under it. The slots hold whole-fleet `Vec`s, and freeing six
figures of rows under a `std` mutex on an async worker stalls every other reader of the
slot for the length of the free. `clear_last_result` and `store_last_result_if_current`
do the same for the result slot.

This was written out four times: lookups, the device inventory, and the two
current-patch families. Prose did not keep the copies in step. `lookups_epoch`'s own
field comment read "Mirrors `devices_epoch`/`current_epoch`, which the lookups cache
was missing" — the epoch check was absent there until someone noticed, and the gate
was never added at all, so concurrent callers on a cold cache each paged the reference
lists independently. Both are now structural: a fifth cache cannot be declared without
the protocol, because the protocol *is* the type.

`AppState` went from 18 hand-managed synchronization primitives (8 `std` mutexes,
3 `tokio` mutexes, 6 `AtomicU64`, 1 `AtomicBool`) to 8 plus four `TenantCache`
fields.

**`last_result` deliberately stays outside it.** Its protocol is *generation*-gated,
not TTL-gated: `begin_query` issues a `QueryToken` claimed before any fetch and
redeemed at the store, so a superseded result is dropped by ordering rather than by
age. It also carries the sort/group memos, which are derived from exactly those rows.
Forcing it into `TenantCache` would mean a TTL it does not have and a value type that
is really a small state machine — see the sections above.

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
`commands::patches::summary_for` refuses to hand back a renderable summary, because the frontend's
guard can arrive too late — `clear_session()` bumps `query_seq` only once the session-ending
command replies, and the query's reply can land first, so returning the summary painted the
previous tenant's rows over the new tenant's empty cache while paging and export read the miss.
The rule: return a summary only when the rows behind it are readable.

## A tenant switch, a sign-out, a sign-in and a re-authorization all clear the frontend

`save_settings` reports `tenant_changed` on its `SettingsView` (both halves of the tenant key —
instance *and* client id), and `apply_settings_view` calls `clear_session()` and resets the
org/location/role/group scope ids (they belong to the previous tenant's lookups). `clear_session` drops
everything `commands::auth::clear_session_state` drops backend-side — the result, the job list and
any pending confirmation — and the sign-in, sign-out and Re-authorize handlers call it too.
Without it the previous session's rows stayed rendered against a cache that had already been
dropped: Next page came back blank under "Rows 101–200 of N" and Export said "Run a query before
exporting" beside a visible table — the same divergence one layer up.

## Paging commands return empty on a miss, never an error

The three paging commands all return empty on a cache miss. A miss is a normal transient (tenant
switch, sign-out, superseded query); the frontend already renders its own empty state from the
absent result. `get_device_rows` (several devices' rows in one pass, `rows::rows_by_device`, the
post-refresh selection prune's read) follows the same rule, with one distinction the caller
needs: an empty *reply* is a miss, so the prune keeps the whole selection; an empty *entry* is a
device the result no longer lists, so its ticked rows go.

`device_detail` (the device drill-down) follows the same rule: `None` on a miss, which the dialog
renders as "not in the current results". It is a read over a handle (`current_result_handle`), its
row scan runs on `spawn_blocking`, it caps the rows at the paging limit and reports the total, and
it is never memoized or cached itself — the frontend reloads an open drill-down whenever a new
result lands, and a stamp (`QueryState.drill_seq`) drops a response that a newer click or reload
has overtaken.

`get_patch_rows` also takes an optional `sort` (`rows::RowSort`), applied through the memoized
order described below — the cached rows themselves are never reordered; their canonical
severity/org/device order feeds the export and the summary's inline first page.

## Both derived views are memoized inside the cache slot

`CachedResult` carries `groups: Option<(GroupBy, …)>` **and** `sorted: Option<(RowSort,
Arc<Vec<u32>>)>`. `rows::sort_order` builds an index permutation; `rows::page_rows` slices it (or
the cache order when `None`, so an unsorted fleet never materializes an identity permutation).
Paging a sorted view once re-sorted every cached row on **every page request**, under the lock the
export also takes. Both memos live *inside* the slot, so replacing or clearing the result drops
them in the same operation and there is no second staleness protocol to get wrong.

**The memo is read and written under the lock, but built outside it.** `AppState::sort_memo` /
`group_memo` return a `Memo { result, memo }` — an `Arc` handle plus the memo if one exists — for
two refcount bumps under the mutex. On a miss the command builds the order or grouping on
`spawn_blocking` over that handle and offers it back via `store_sort_memo` / `store_group_memo`,
which keep it **only if `Arc::ptr_eq` says the slot still holds the same result** (a query can
land, or a sign-out clear the slot, mid-build; a permutation over the old rows would page the new
ones in a meaningless order). The first version built the memo inside the lock, from an `async`
command with no `.await`: the first sorted page of a six-figure fleet ran the whole sweep on a
tokio worker holding the mutex every paging command and the export take. `get_patch_group_members`
scans every row for its key, so it too runs on a handle in `spawn_blocking`; so does
`get_device_rows`, one scan for every device it is asked about.

## Grouping is backend-side too

The Patches tab's *By device* / *By patch* / *By product* modes go through `get_patch_groups` (headers + total,
`rows::build_groups` memoized, then `rows::slice_groups`) and `get_patch_group_members` (one group's rows, `rows::group_member_page`).
The frontend only ever holds one page, so it cannot group a fleet it has never seen — never
regroup `page_rows` client-side. Group headers carry **no** members: a by-patch group can span the
whole fleet, so members load on expand, capped at `GROUP_MEMBER_LIMIT`. `rows::group_key` is the
identity the frontend echoes back, so no per-request state is kept backend-side and a stale key
matches nothing. `demo.rs` mirrors `group_key`/`build_groups` by hand for the browser demo — keep
the two in step.

### *By product* folds versions, and falls back to the patch key

NinjaOne's third-party feed folds the version into the title and has no product-name field, so
by patch every Chrome build was its own group. `DeviceSoftwarePatch.productIdentifier` (a uuid
shared by every version) is now bound on `Patch` and interned onto `PatchRow`, and
`GroupBy::Product` keys a software row with one by it: `SOFTWARE␟<id>`, **two** fields where a
patch key has three, so `GroupKeyMatcher` tells them apart by shape and neither can pass for the
other. Every other row — every OS patch, and a software record missing its identifier — keeps its
exact by-patch key, and under product mode a row that *has* a product never matches a patch key,
so members still partition the rows. The label is derived (`rows::product_display_name`): each
member title loses one trailing version token (`strip_version_token` — starts with a digit, holds
a `.`, only `[A-Za-z0-9.-_+]`, so "Office 2016" stays whole) and the most common result wins,
ties broken case-insensitively so the label does not depend on row order; the sublabel counts the
versions. The demo carries a copy of both helpers (`util::product_display_name`), pinned by the
regenerated `web-rs/tests/backend-grouping.json` (`byProduct`, `keysByProduct`).

Rows whose patch record carried no `deviceId` get `PatchRow.device_id = rows::ORPHAN_DEVICE_ID`
(0 — NinjaOne ids start at 1). They still share one *By device* bucket, but its `device_id` is
`None`, so it offers nothing to dispatch against, and neither `build_groups` nor `build_failures`
counts the sentinel as a device: it used to be one phantom machine in every patch group and failure
count its records touched, and a device group whose `device_id` was `Some(0)`.

## Compact aggregates ride in the summary, not the rows

Fleet-wide distributions the frontend charts/failure tab need — `failures` (FAILED-install
rollup, `build_failures`), `severity_by_org` (`build_severity_by_org`), `age_buckets`
(`build_age_buckets`), `worst_devices`/`offline_backlog` (`build_device_backlogs`),
`time_to_install` (`build_time_to_install`), `approvals` (`build_approval_backlog`), plus the
`sla_policy` they were aged under — are computed backend-side in `rows/` and carried on **both**
`QueryResult` (cached; the HTML report reads it) and `QuerySummary` (IPC; the dashboard reads it).
They're bounded (one entry per failing patch / per org / 5 buckets / 25 devices), so they ship
whole rather than paged — except `approvals.stuck_devices`, which is one entry per *device* and so
is capped on the summary (`ApprovalBacklog::capped(STUCK_DEVICES_SUMMARY_CAP)`, oldest first,
totals kept) while the cached result keeps every one for the workbook.
Add such a field in lockstep: `QueryResult` + `QuerySummary` + clone in `QuerySummary::from_result`
+ the `web-rs/src/types.rs` mirror + the demo's `assemble`, then regenerate
`web-rs/tests/backend-ipc.json` with the field filled — `types::tests` decodes the summary through
the mirror, so the backend `QuerySummary` ⇄ frontend `QueryResult` (`web-rs/src/types.rs`) shapes
cannot drift apart unnoticed. See
[frontend.md](./frontend.md#ipc-shapes-are-pinned-by-a-backend-generated-fixture).

The documented exceptions are `QueryScope` and `instance`, which live on `QueryResult` only —
export provenance the frontend already has — see
[compliance.md](./compliance.md#both-exports-state-the-facets-from-rowsqueryscope).

`changes` (`changes::RunChanges`) rides the same way, but is filled *after* `assemble_result`:
it needs the previous run's snapshot, so `query_patches` loads it on a blocking thread between
the join and the summary — from the file, or from the snapshot this process wrote last when the
scope is the same (the auto-refresh case), which skips the read and the parse. Its lists are capped at `CHANGE_LIST_LIMIT` so it stays
compact — see [compliance.md](./compliance.md#changes-since-the-previous-comparable-run).

## Whole-fleet prefetch + client-side scoping

The device inventory and current patches (OS + 3rd-party) are fetched **whole-fleet** (no `df`)
and cached in `AppState` (`fleet_devices_cache`, `DEVICE_TTL` ~15 min since devices change rarely;
the current patches in `fleet_current_os` / `fleet_current_sw`, refreshed on `force_refresh` or
past `CURRENT_PATCHES_TTL`). `run_query` then scopes them to the selected identity facets
(org/location/role/group/class) **client-side** via `FilterParams::device_allowed` — so changing
org/location/role/type/severity re-filters the cache with **no** round trip. A device group is the
exception: its membership is one small read per selected group per query (see
[filter.md](./filter.md#device-groups-are-read-per-query-and-fail-closed)). This is why
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
cannot catch this, it is the same tenant. Each slot's epoch is sampled on entry to
`get_or_fetch` and re-read **under the slot lock** at the store; the invalidators bump the epoch
*before* clearing the slot, so the two orderings both lose the write. Separately, each cache has a
`tokio::Mutex` fetch gate (mirroring `AuthState::refresh_lock`) with a re-check after acquiring:
on a cold cache two callers would otherwise page the entire inventory / third-party feed
independently. Per family, so an OS-only query never waits on an in-flight third-party fetch.

The frontend no longer overlaps queries with each other (`run_decision` in
`web-rs/src/app/util/query.rs` refuses a silent auto-refresh tick and queues a manual Run while
one is in flight), so the overlap these gates meet in practice is a query against `plan_action`
(which reads `fleet_devices`) or `cached_current_patches`. The query generation gate stays as the
backend's own defence against a stale or hand-built frontend.

The job poller invalidates **at most once per tick**, however many mutating kinds settle in it —
every bump makes a whole-fleet fetch in flight discard its rows. It is deliberately not debounced
across ticks, and there is no "mark stale without bumping the epoch": a fetch started before a
settle must never store.

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

The pull runs in the same `join!` as the cache legs, which all finish even when one fails so the
caches are warm for the retry. The uncached legs (install history, device-group members) do the
opposite: they abort when a devices, current-patch or lookup leg fails, because the query is
already lost and minutes of sequential paging would only hold its error back. A `watch` flag
carries the failure, so a leg that starts waiting after it was raised still sees it; the error
surfaced is still the cache leg's.

### Scoping borrows, never clones

`assemble_result` filters the cached `Arc`s into one `Vec<rows::PatchRecord>` — a borrowed view
per scoped record (the `&Patch`, its parsed severity, its pending verdict and its effective first
seen), OS first — and the rollups (`build_device_summaries`, `build_compliance`,
`build_compliance_by_os`, `build_severity_by_org`, `build_age_buckets`, `build_device_backlogs`,
`build_approval_backlog`) plus `PatchSource.records` all take `&[PatchRecord]`. A whole-fleet
third-party feed runs to six figures and each `Patch` owns **eight** `Option<String>`s, so cloning
the scoped subset costs millions of allocations per query for data the cache already owns and
outlives. Keep new rollups on `&[PatchRecord]`; don't reintroduce an owned `Vec<Patch>` to make a
signature more convenient, and don't read `Patch::collected_at` for an age — the view's
`first_seen` is the one decision about which time a record is aged from.
