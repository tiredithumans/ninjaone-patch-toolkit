# What a compliance number means

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/rows/` (`compliance.rs`, `rollups.rs`, `scope.rs`, `join.rs`),
`src-tauri/src/export.rs`, `src-tauri/src/report.rs`, `src-tauri/src/commands/patches.rs`,
`web-rs/src/app/util/`.

The rollups describe a *narrower* population than `devices_total`, and every surface has to say
so rather than leave two device counts side by side.

## One population for every fleet-health rollup, via `rows::rollup_device`

The device must be in the scoped inventory, online, **and** something NinjaOne patch management
covers (`Device::is_patchable` — an allow list of the Windows/macOS/Linux `nodeClass` values,
`model::PATCHABLE_NODE_CLASSES`; a device with no class is kept). Offline devices are excluded
because they report no current patch records, so a zero pending count says nothing about them;
switches, printers, hypervisors and cloud monitors are excluded for the same reason — they are
online, carry no patch records, and would otherwise score *compliant*, so 100 servers plus 100
network devices read 25 points better than the servers did. The allow list fails toward exclusion
(which every surface states as a count) rather than toward a silently higher percentage.

`accumulate_compliance` applies it to the device loop *and* the patch loop (the patch loop once
skipped it, so an org whose devices were all offline read "0 devices · 100% compliant · 45 pending
Critical/Important", and an orphan patch opened its own zero-device `(unknown)` org).
`build_severity_by_org` and `build_age_buckets` apply it too: the HTML report prints those two
charts directly beneath the compliance sections, under a header stating "Compliance covers online
devices only (N offline devices excluded)", so charts that skip the exclusion silently re-admit
the excluded population — the gap being exactly the offline backlog, and unrecoverable from the
page. `build_age_buckets` therefore takes `devices_by_id`; taking only the patches made it the one
rollup structurally *unable* to apply the exclusion. A new rollup over the current feed goes
through `rollup_device` too — `severity_and_age_rollups_cover_the_same_devices_compliance_does`
pins the three against each other.

## `devices_offline`, `devices_unpatchable` and `patch_families` ride on `QueryResult`/`QuerySummary`

So the note can be stated: "Compliance covers online Windows, macOS and Linux devices only (N
offline and M non-patchable devices excluded)". `devices_unpatchable` counts *online* devices
only, so `devices_total − devices_offline − devices_unpatchable` is the compliance denominator
and the three numbers reconcile. `rows::compliance_scope_note` builds the sentence; the
Compliance tab (`ComplianceScopeNote`), the HTML report header and both workbook compliance sheets
print it, and the frontend `util` module mirrors it (the crates share no code — both sides are
tested). The detail sheet carries an **Offline** column for the same reason: a sheet asserting "N
offline devices excluded" has to let the reader reproduce the denominator, and `PatchRow.offline`
was already there (the in-app table draws its "offline" chip from it).

## Both exports state both clocks

`generated_at` is the join/rollup clock; `data_fetched_at` is when the fleet data last came from
NinjaOne, and a re-filter recomputes over a warm cache with no round trip — so an export stamped
only with `generated_at` dates the fleet to the moment someone pressed a button. The report header
prints both; the workbook's **About** sheet carries them plus the scoped/offline device counts and
the detail-row total (`export::WorkbookMeta`).

## Both exports state the facets, from `rows::QueryScope`

Built by `build_query_scope` in `assemble_result` out of the `QueryPlan` the fetch actually ran
under — **not** from the request and **not** from the frontend's `AppliedFilters`. Those describe
what was *selected*; the block has to describe what the query *did*, which is the same
backend-re-derives-rather-than-trusts rule the write path follows. `AppliedFilters` is
frontend-only and never crosses IPC anyway. Without it two workbooks off one fleet — one scoped to
a single org and CRITICAL-only, one unfiltered — are indistinguishable once saved, while every
number in them describes a different population.

- `QueryResult`-only, deliberately **not** on `QuerySummary`: the frontend has its own chip row,
  so a second copy over IPC would be a wire field with no reader. This is the documented exception
  to the compact-aggregates lockstep rule in
  [query-cache.md](./query-cache.md#compact-aggregates-ride-in-the-summary-not-the-rows).
- **Two tiers, and both exports say which is which.** `QueryScope.facets` holds the facets that
  narrow every sheet and section (device scope + `Patch type`); `QueryScope.patch_facets` holds
  the ones that narrow only the detail rows (`Status`, `Severity`, `Search`, the first-seen
  window, the install lookback) — the compliance, severity, age and reboot sections are computed
  from the *unnarrowed* current feed. The About sheet prints them under "Filters (every sheet)"
  and "Patch filters (Patches and Patch Failures sheets only)"; the report under matching
  captions. The in-app Compliance tab already dims those chips with "Ignored on this tab", but a
  workbook that listed `Severity: CRITICAL` beside the Compliance sheet with no such note read as
  a critical-only backlog. A new facet goes in the tier its scope actually has.
- Date bounds are **absolute** (`%Y-%m-%d %H:%M UTC`), with the relative window in parentheses
  when that is the control the operator used — "the last 30 days" silently re-anchors to whenever
  the artifact is read. They are composed backend-side as Unix *seconds*, so they use
  `DateTime::from_timestamp`, not `model::unix_to_datetime` (whose millisecond normalization is
  for values read off NinjaOne records).
- Patch families are stated **once**, as the block's `Patch type` entry — the Type facet and the
  rollups' family scope are the same value, and two adjacent rows saying it read as two things.
- The install lookback is named only when the status selection actually reached the history
  endpoints (`plan.want_installs`), and an unnarrowed query emits an explicit whole-fleet
  sentence: on a printed artifact, missing lines are indistinguishable from a renderer that
  dropped them. `QueryPlan` keeps `statuses` verbatim for this — the two derived `HashSet`s are
  unordered and spelled in NinjaOne's wire vocabulary, so `MANUAL` ⇄ "Pending" would be a second
  place to get the mapping wrong (`PatchStatus::label`).

## The fleet-health rollups *do* depend on the patch-`Type` facet

Only the families a query asked for are fetched at all (see the whole-fleet prefetch — a
third-party feed runs to six figures, so an OS-only query does not page it). That makes
"compliant" mean "no pending OS patches" on such a query. The tabs and the exports name the
families instead of claiming Type is ignored. The `Type` chip is therefore a **device-tier** chip
(`filter_chips` marks it `patch: false`), never struck through on the fleet tabs, and the Filters
panel renders the Type control *outside* the fold that hides the row-only facets there — for a
while the chip said "Ignored on this tab" directly above a banner saying the opposite.

## "Compliant" and "Pending Critical/Important" grade differently, on purpose

Compliant is `pending_count == 0` over patches of *any* severity (`is_pending`), while the two
SLA columns count only rank ≥ Important (`counts_toward_backlog`). A row can legitimately read
"10 devices · 4 compliant · 0 pending Critical/Important".

## `rows::is_pending` is an exclude list

A current-feed record is pending unless it is `REJECTED` or `INSTALLED`. `status` has no enum in
the spec and is not required on `DeviceOSPatch`/`DeviceSoftwarePatch`; the feed's description says
"no installation attempts" but the same endpoints are titled "Pending, **Failed** and Rejected …
report" (`getPendingFailedRejected*`), so `FAILED`, an untyped record, or a value this crate has
never seen can all arrive there. The allow list this replaced (`MANUAL | APPROVED | None`) scored
such a device *compliant* and dropped its most urgent patch from every rollup — the wrong
direction to fail in, and the opposite of what `is_aged` does with an undated patch. Two things
keep the rows in step with the rollups: `QueryPlan::current_status_set` carries **every**
selected status (it only narrows the rows built from the current feed; the rollups take the
unnarrowed feed), so a FAILED current record shows under the Failed selection; and
`assemble_result` gives both current sources `status_override = MANUAL`, so an untyped record
matches the Pending selection and renders as PENDING instead of being counted by the Compliance
sheet and missing from the Patches sheet.

## A percentage never rounds up to 100

`rows::format_pct` (and its `web-rs` mirror) caps anything below 100 at 99%, and `pct_cell` does
the same at one decimal. Plain `{:.0}%` prints "100%" from 99.5% up, so 199 of 200 devices
patched reads as a clean fleet — the one rounding error here that changes what an operator does.

## Enumerate bands through an accessor list, never by matching a label string

`rows::SeverityCounts::BANDS` and `charts::SEV_BANDS` both pair each band with the function that
reads it, and their totals derive from that list. A version that matched on the display label
with a `_ => c.unknown` fallback let a renamed band silently draw Unknown's count twice and
overflow the bar. See [severity.md](./severity.md).

## Table headers come from `rows::TableColumn` spellings

The Leptos tables are hand-written and are not wired to `COLUMNS`, so they are kept spelled
identically by review: "Compliance %", "Pending Critical/Important", "Aged (past SLA)", "Device
Role", "Pending Patches", and the Failures table's seven columns including "Patch Type".

`rows::TableColumn<T>` is the shared table definition. Every table rendered from a cached
`QueryResult` — `FailureGroup::COLUMNS`, `DeviceSummary::COLUMNS`, `ComplianceBucket::COLUMNS`,
`OsCompliance::COLUMNS`, plus `export.rs`'s own `DETAIL_COLUMNS` — pairs each header with the
accessor that fills it, so a column is one declaration rather than two lists agreeing by
convention. `export.rs` renders all five through one `write_sheet` and contributes only the width
arrays, each length-tied to its `COLUMNS.len()`; `report.rs` renders through one `write_table`.
Before this the two had diverged: the report dropped `Patch Type` from the failures table, and
hardcoded the reboot table's headers as "Role"/"Pending patches" against the workbook's "Device
Role"/"Pending Patches".

## There is no patch release date in the NinjaOne API

Grep the spec: `releaseDate` appears **zero** times. `DeviceOSPatch` / `DeviceSoftwarePatch`
carry only `installedAt` ("Installation attempt timestamp") and `timestamp` ("Date/Time when
data was collected/updated"); the non-`Device` `OSPatch`/`SoftwarePatch` variants carry neither.
`Patch::collected_timestamp` (alias `timestamp`, read via `first_seen_at()`) is therefore
**detection time, not publication time**, and everything derived from it —
`PatchRow.first_seen_ts`/`first_seen_date`, the SLA `aged_critical` rollup, `build_age_buckets`,
and the `detected_within_days`/`detected_after`/`detected_before` filter window — measures *how
long we have known about the patch*. The UI says so ("First seen", "Pending past SLA", "Pending
patch age (since first seen)"); keep the naming honest if you touch these.

This was once a field named `release_timestamp` aliasing a `releaseDate` that never binds, so the
SLA rollup compared *now* against an always-recent timestamp and reported ~0 breaches on any
fleet — and the wiremock fixtures fed `releaseDate`, so CI proved only that the aliasing worked.
**Fixtures must emit `timestamp`.** Undated pending patches get their own `Unknown` age bucket
rather than inflating `180+ days`; they still count as aged in the SLA rollup (`unwrap_or(true)`
— can't prove recent).

## `PatchRow` shares its repeated strings; it does not own them

Device/org/location/role/OS names, patch titles, KBs and statuses are `Arc<str>` handed out by a
per-join interner (`rows::Interner`), the device-derived half is resolved once per device
(`rows::DeviceLabels`) rather than once per patch, and `patch_type`/`severity` are `&'static str`
because both vocabularies are fixed. The cached `QueryResult` is the app's largest live
allocation, and it once held one owned `String` per field per row for a few thousand distinct
values. `FailureGroup` and `PatchGroup` carry the same shared strings so the rollups are refcount
bumps. All of it serializes to plain JSON strings, so `web-rs/src/types.rs` still mirrors them as
`String` — `serialized_shapes_carry_every_frontend_required_key` asserts the wire *types*, not
just the keys, because the two crates share no code.

## Installed/Failed vs current patches (status routing)

Per the official spec, the current `/queries/{os,software}-patches` feed returns only patches
"for which there were **no installation attempts**" (statuses `MANUAL`/`APPROVED`/`REJECTED`),
while `/queries/*-patch-installs` returns the install **history** — "successful **and** failed"
records (status `INSTALLED`/`FAILED`). So **both** `Installed` *and* `Failed` are install
*results* and must route to the install-history endpoints over the lookback window
(`settings.install_window_days`, overridable per query); only `Pending`/`Approved`/`Rejected`
narrow the current feed. `PatchStatus::is_install_history()` encodes this. Routing `Failed`
*only* to the current feed was a real bug — a FAILED query returned nothing, because failed
installs live in the history. (A `FAILED` record that *does* arrive in the current feed is still
counted and shown — see `is_pending` above; the two are not exclusive.) Current patches are
**always** fetched regardless of the status filter (they drive compliance % and pending/reboot
counts). See `commands/patches.rs`.

### Install-status pushdown

The `*-patch-installs` endpoints honor a server-side `status` (`FAILED`/`INSTALLED`). When the
operator requests **exactly one** install status, `run_query` passes it to
`fleet_*_patch_installs` so a FAILED-only (failure-dashboard) query doesn't download the window's
successful installs just to drop them; with **both** requested it's left unset (both records are
needed). The client-side `install_status_set` narrowing in `build_rows` stays as a backstop. The
current feed is **not** status-filtered server-side — narrowing it would starve the
compliance/severity/age rollups, which need the full `MANUAL`/`APPROVED`/`REJECTED` set.

### The lookback window is re-applied client-side

`installedAfter` is typed only as `string` in the spec with no stated format. Unix seconds is
what the widely used community PowerShell module sends and what this app has always sent, but the
response carries no evidence the bound was honored, and both exports print "Install history since
<date>" on the strength of it — so `assemble_result` drops install records whose `installedAt`
predates `plan.installed_after` (undated records are kept; the window cannot prove them out).
