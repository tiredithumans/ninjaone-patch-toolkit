# What a compliance number means

Contract lines: [AGENTS.md → Conventions & gotchas](../../AGENTS.md#conventions--gotchas).
Code: `src-tauri/src/rows/` (`compliance.rs`, `rollups.rs`, `backlog.rs`, `install_time.rs`,
`scope.rs`, `join.rs`), `src-tauri/src/settings.rs` (`SlaPolicy`),
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
pins the three against each other, and the worst-devices list alongside them.

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

## One per-device rollup for the Devices sheet and the drill-down

`DeviceSummary` carries each in-scope device's share of the fleet-health numbers — pending by
severity band, pending past SLA (`aged_critical`), failed installs and NinjaOne's `lastContact` —
filled by `rows::apply_device_health` from the **same unnarrowed current feed** and the same
`is_pending` / `counts_toward_backlog` / `is_aged` predicates the compliance rollups use. The
workbook's **Devices** sheet (`DeviceSummary::DEVICE_COLUMNS`) and the in-app drill-down both read
it, so neither can grade a device differently from the Compliance sheet that counts it.

- **Every in-scope device is listed, and `RollupScope` says whether it is in the
  [`rollup_device`](#one-population-for-every-fleet-health-rollup-via-rowsrollup_device) population
  and, if not, why** (`Excluded (offline)` / `Excluded (non-patchable)`). The scope note says "N
  offline and M non-patchable devices excluded"; this is where a reader finds them by name. Their
  count cells are **blank, not zero**: an offline device's zero means "unknown", and a zero would
  read as "clean" — the misreading the exclusion exists to prevent. The drill-down shows the reason
  instead of the counts for the same reason.
- **Band columns come from `SeverityCounts::BANDS`** (one `band_cell::<I>` per band); their
  headers are a hand-written `DEVICE_BAND_HEADERS` array because a `const` cannot concatenate the
  labels — `device_band_headers_follow_the_bands` pins the two together.
- **`failed_installs` is `None` unless the query asked for the Failed status.** Failures live in
  the install history, which is only fetched for an install status; without it "0 failed" is not
  something the result can know. The sheet leaves the cell blank and says so in a footnote.
- **Order is (organization, device name, id)**, case-insensitive (`sort_device_summaries`, applied
  in `assemble_result`), so two runs list identically and same-named devices still sort stably.
- **`lastContact` is display-only.** Reachability is the vendor's own `offline` flag; the timestamp
  only lets a reader judge how stale an offline device is. It is normalised like the patch
  timestamps (`Device::last_contact_at` → `unix_to_datetime`), so a millisecond value cannot read
  as year 58000.

The drill-down's rows are different on purpose: they are the device's **Patches-tab** rows, so
every patch filter applies, and the dialog says so beside counts that ignore them.

## The workbook writes real date-times, in UTC

`TableCell::DateTime(Option<i64>)` carries an instant as Unix seconds; `export::write_cell` writes
it as an Excel date-time (`ExcelDateTime::from_timestamp`, number format `yyyy-mm-dd hh:mm`), so
First Seen, Installed Date, Latest Failure, Last Contact and the About sheet's two clocks sort and
filter as dates. Written as text they sorted as strings, and "first seen before May" was a string
compare. A date cell has no zone, so the About sheet carries a **Time zone** row stating that every
time is UTC. An instant outside Excel's 1900–9999 range degrades to its text spelling rather than
failing the export; `None` leaves the cell empty. The text renderers — the HTML report and the CSV —
print the same instant through `rows::utc_text` (`2026-05-01 09:15 UTC`), the spelling the rows
already carry, so all three artifacts show one time the same way.

## The CSV export

`export_csv` writes the Patches sheet's `DETAIL_COLUMNS` over every cached row (no row limit —
CSV has none) through `csv_export::write_csv`, a small hand-written writer rather than a
dependency: UTF-8 with a BOM (without it Excel reads the file in the ANSI code page and every
non-ASCII name turns to mojibake), CRLF records, and RFC 4180 quoting — every text field quoted,
embedded quotes doubled.

- **Formula injection (OWASP "CSV Injection").** A text cell beginning with `=`, `+`, `-`, `@`,
  tab or CR gets a leading `'`, so a device or patch title like `=HYPERLINK(…)` is displayed rather
  than evaluated when the file is opened in a spreadsheet. `-` is on the list, which is why the
  guard is applied to `TableCell::Text` only: `Count`/`Number` cells are written bare and stay
  numeric, negative values included. The visible apostrophe on a text cell that merely starts with
  `-` is the accepted cost.
- **Provenance lives in the file name and nowhere else.** CSV has no comment or metadata syntax,
  and a preamble row breaks every importer, so the proposed name carries the device scope (the
  device facet values, or `whole-fleet`), the status selection and both clocks:
  `ninjaone-patches_whole-fleet_pending_data-20260502T0840Z_generated-20260502T0915Z.csv`. The other
  patch filters do not fit in a file name; the workbook's About sheet is the artifact to share when
  they matter, and the button's tooltip says so.

## Both exports state both clocks

`generated_at` is the join/rollup clock; `data_fetched_at` is when the fleet data last came from
NinjaOne, and a re-filter recomputes over a warm cache with no round trip — so an export stamped
only with `generated_at` dates the fleet to the moment someone pressed a button. The report header
prints both; the workbook's **About** sheet carries them plus the scoped/offline device counts and
the detail-row total (`export::WorkbookMeta`). The CSV carries both in its proposed file name (see
above).

Both also state the NinjaOne **instance** (`QueryResult::instance`, stamped at assembly from the
settings snapshot the query ran under — `QueryResult`-only like `QueryScope`, since the frontend
knows its own instance), the **app version** (`export::APP_VERSION`, the backend crate's
`CARGO_PKG_VERSION`), and the **SLA policy** (`SlaPolicy::describe`). Two workbooks from two
tenants, or from before and after an SLA change, were otherwise indistinguishable once saved.

## The SLA is per severity band

`Settings.sla_days` is the default window; `Settings.sla_by_severity` (`SlaBySeverity`) holds an
optional override per band. It has no `Unknown` field: an unmapped severity carries no urgency to
set a target for, so it always takes the default. Every field is `#[serde(default)]`, so a
settings file (or frontend) from before this existed loads with no overrides and ages exactly as
before. Save-time validation rejects an override outside `1..=MAX_WINDOW_DAYS` (blank is always
valid); `Settings::sla_policy()` clamps again, because a hand-edited file reaches
`Duration::days`, which panics on overflow.

- **Cut once per query, per band.** `rows::SlaCutoffs` precomputes one cutoff per `Severity`
  (indexed by `rank()`, unique per variant) and `is_aged` reads the patch's own band. Undated
  patches still count as aged.
- **Two "past SLA" figures, on purpose.** The compliance tables' "Aged (past SLA)" stays the
  Critical/Important subset of the column beside it ("Pending Critical/Important"), so the two
  still reconcile. The worst-devices and offline lists' "Past SLA" counts *every* pending record
  against its own band's window — that is what the overrides for Moderate/Low/Optional are for.
  With the default policy (30 days everywhere) an old Optional backlog therefore counts; set a
  longer window for the low bands if that is not the target.
- **The result carries the policy it was computed with** (`QueryResult`/`QuerySummary`
  `sla_policy`). An export after a settings change states the numbers' policy, not the new one,
  and the Compliance tab prints it for the same reason.
- **Changing it clears no cache.** The policy is read from the settings snapshot when a query is
  assembled; only an instance/client-id change invalidates fleet data. The next **Run query** — a
  re-filter over the warm cache, no refetch — reflects the new policy; the result on screen keeps
  its own until then.
- The run history's `aged_critical` is recorded under whatever policy each run used, so a trend
  across a policy change moves for that reason alone.

## Worst devices and the offline backlog

`rows::build_device_backlogs` makes one pass over the unnarrowed current feed and returns two
capped (`DEVICE_BACKLOG_LIMIT` = 25) lists, each with the total that qualified so every surface
can say "top 25 of N":

- **`worst_devices`** is the `rollup_device` population — the devices the compliance table
  counts — ranked by past-SLA records, then by the most urgent breakdown
  (`SeverityCounts::cmp_urgency`: more Criticals wins, then Important, … down `BANDS`), then by
  device id. The breakdown order already implies the larger total wherever the bands differ, so
  total pending needs no step of its own; the id makes the order total (the accumulator is a
  `HashMap`).
- **`offline_backlog`** is its deliberate complement: scoped, patchable devices that are offline
  yet still appear in NinjaOne's current feed with pending records. The "offline devices report
  no current patch records" line above is why they are excluded from the rollups — a zero says
  nothing about them — but the whole-fleet feed does carry whatever was last collected for them,
  and dropping it everywhere made that backlog invisible. The records may be stale; every surface
  says so.
- **No last-contact time.** `Device` deserializes no last-contact field, and the committed spec
  digest (`docs/api/ninjaone-surface.md`) does not cover the device schema, so none is invented.
  The list shows `latest_collected` instead — the newest `timestamp` on the device's pending
  records, NinjaOne's "collected/updated" time — labelled "Latest patch data collected", not
  "last contact".

## First seen → installed is indicative only

`rows::build_time_to_install` measures `installed_ts − first_seen_ts` over the INSTALLED detail
rows, overall, by organization and by severity (most urgent band first): median (mean of the two
middles for an even count) and a nearest-rank p90 (always an observed value), with the sample
size. A record missing either time, or installed before it was first seen, is counted in
`excluded_records` and skipped.

- **The caveat is the point.** `timestamp` is "Date/Time when data was collected/updated"; on an
  install-history record that can sit close to the install itself, so the figure can understate
  the real lag. Every surface labels it "First seen → installed" and prints the caveat.
- **Built from the detail rows, like the failures rollup**, so it follows the patch facets
  (status, severity, search, first-seen window, install lookback) as well as the device scope.
  The exports list it with the patch-filter tier; the Compliance tab — whose banner says patch
  filters are ignored there — says this section is the exception.
- **Empty says why.** Without the Installed status there is nothing to measure
  (`installs_queried: false`), which `TimeToInstall::empty_reason` (mirrored in `util::sla`)
  distinguishes from "no record had both times". The workbook omits the sheet and says why on
  **About**.

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
  window, the install lookback) — the compliance, severity, age, reboot and device-list sections
  are computed from the *unnarrowed* current feed. The About sheet prints them under "Filters
  (every sheet)" and "Patch filters (Patches, Patch Failures and Time to Install sheets only)";
  the report under matching captions. The in-app Compliance tab already dims those chips with "Ignored on this tab", but a
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
  endpoints (`plan.want_installs`), as `Install history since <bound> (last N days)`; a custom
  absolute range prints `since` and `until` with no parenthetical (`rows::InstallWindow`), and a query with no **device-tier** facet emits an explicit
  whole-fleet sentence: on a printed artifact, missing lines are indistinguishable from a renderer
  that dropped them. The sentence sits in `facets` (every sheet), so only a device facet
  (`FilterParams::has_identity_scope`) may remove it — a severity- or search-only query once
  dropped it, leaving a CRITICAL-only export's Compliance sheet with no statement of its
  population. Blank (whitespace-only) needles are no facet at all, as in `prepare()`.
- `QueryScope.device_scoped` and `QueryScope.fingerprint` feed the run history
  (`history::RunRecord::scoped` / `scope_key`). `scoped` once counted `facets` entries, so every
  unfiltered run (whole-fleet line + patch type) read as scoped; and a bool cannot tell org A's
  runs from org B's. The fingerprint is a canonical JSON spelling of every facet (ids sorted,
  needles trimmed and lowercased, relative windows as `30d` because the absolute bound moves each
  run); records from before it read back with an empty `scope_key`. The install lookback is not in
  it for the same reason; an *absolute* install range is a different question and is appended as
  an `installed` part — only when present, so every older fingerprint still matches its line. `QueryPlan` keeps `statuses` verbatim for this — the two derived `HashSet`s are
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

## Changes since the previous comparable run

`changes.rs` answers "what moved since last time" by diffing each query against a snapshot of the
previous one, and three decisions shape what the numbers mean.

- **Comparable is tenant + facets.** The snapshot file is keyed by a hash of the tenant (instance
  + client id, from the `QueryToken`, i.e. the tenant the rows were *fetched* under) and
  `changes::scope_key`: `QueryScope::fingerprint` (already canonical and order-insensitive) plus
  the patch families and — only when installs were fetched — the install lookback, the two
  inputs the fingerprint leaves out. The tenant and scope are also stamped inside the file and
  checked on load, so a hash collision or a copied file cannot diff across tenants. Org A against
  org B would report org A's whole backlog as resolved.
- **It diffs the detail rows, not the rollup population.** The rows carry every facet (status,
  severity, search, first-seen window), so the diff describes exactly what the Patches tab lists;
  a CRITICAL-only scope reports critical changes. The price is that a status selection without
  Pending/Approved measures no new/resolved, and one without Failed cannot tell a failed install
  from a resolved one (a failed patch leaves the current feed). `RunChanges` carries
  `tracks_pending` / `tracks_failed` and every surface prints the resulting caveat
  (`RunChanges::notes`, mirrored by `util::changes_notes`) instead of showing zeros that read
  as "nothing changed". A FAILED row is its own category, not pending, and "resolved" excludes
  anything failed now, so one failure is never listed twice.
- **Resolved needs display text the current rows do not have.** A resolved patch is absent from
  this run by definition, so the snapshot stores the pending set's device names and patch titles
  (interned: device and patch tables, items as index pairs — ~11 bytes a pending row) rather than
  bare hashes. Failed items use the same form; they are few.

Patch identity is `changes::patch_key`: an OS patch is its normalized KB, anything else its
lowercased title. It is the one place to adopt `productIdentifier` for third-party patches.

The baseline advances only when a result **wins the cache** (`StoreOutcome::Stored`); a
superseded run is older than the one that won, and writing it would roll the baseline back. The
write is atomic (temp + rename, 0600) on a blocking thread, and `query_patches` awaits it before
returning (`save_baseline_if_stored`). It used to be detached, so a query started right after
(an auto-refresh tick, a quick re-run) could load the baseline before the save landed and diff
against the run before last. The directory keeps the 20 most
recently written scopes within 64 MB, always keeping the newest. A run past a million items is
not written and says so (`too_large`). Consequence worth knowing: every stored run is the next
baseline, so re-running the same scope over a warm cache reports "no changes since" the run
seconds earlier — "since the previous run", literally.

The Trend tab's per-organization table reads `history::RunRecord::orgs` (the Compliance tab's
per-org rows plus each org's pending count), capped at the 100 largest organizations per line so
an MSP's history file stays bounded; `read_run_history` returns that detail only for the newest
200 lines, since the table compares only the newest run with the previous comparable one. Cards
lead with the change against that previous run; the change over the whole series is the footer.

## Awaiting approval and approved-not-installed are read from the vendor status

Both compliance rollups carry `awaiting_approval` (vendor status `MANUAL`, NinjaOne's "Pending") and
`approved_not_installed` (`APPROVED`), over the same `rollup_device` population and across **every**
severity — the split describes the workflow, not the urgency, unlike the Critical/Important SLA
columns beside it. They matter because the two stall for different reasons: `MANUAL` waits on a
person, `APPROVED` on the agent, and `is_pending` scores both the same.

The split reads `Patch.status` off the **cached record** (`rows::approval_state`), never a row's
display status. `assemble_result` gives the current sources `status_override = MANUAL` so an
untyped record matches the Pending selection — but that override only ever reaches `build_rows`;
the rollups take the raw feed. Reading the row status would file every untyped record as "awaiting
approval", a claim the vendor never made. So an untyped, `FAILED` or never-seen status is pending
(it still breaks compliance) and in **neither** column: the two can sum to less than the pending
count, and that gap is the records whose workflow state NinjaOne did not say.

`QueryResult.approvals` (`rows::ApprovalBacklog`) carries the fleet totals — equal to the columns
summed, one population — plus the **stuck approvals**: per device, the `APPROVED` patches first seen
more than the default SLA days (`SlaPolicy::default_days`) ago, oldest first. No operator decision is holding those up, so they point at
agents (never-opening maintenance windows, a broken patch engine) rather than at a backlog. The
threshold is the SLA window because that is already this app's "too long" knob; a second one would
only let the two disagree. An undated approved patch counts as stuck, the same rule `is_aged` uses.
The workbook writes every stuck device on a **Stuck Approvals** sheet with the threshold as a
footnote; the report and the Compliance tab show the oldest ones and say how many more there are.

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
identically by review: "Compliance %", "Pending Critical/Important", "Aged (past SLA)",
"Awaiting Approval", "Approved, Not Installed", "Device Role", "Pending Patches", the Failures table's seven columns including "Patch Type", and the
two device lists (`DeviceBacklog::WORST_COLUMNS` / `OFFLINE_COLUMNS`), and `StuckDevice::COLUMNS`. The in-app first seen →
installed table drops `InstallLatency::COLUMNS`' "(Days)" suffixes because its cells carry the
unit themselves.

`rows::TableColumn<T>` is the shared table definition. Every table rendered from a cached
`QueryResult` — `FailureGroup::COLUMNS`, `DeviceSummary::COLUMNS`, `ComplianceBucket::COLUMNS`,
`OsCompliance::COLUMNS`, `StuckDevice::COLUMNS`, `DeviceSummary::DEVICE_COLUMNS`, plus `export.rs`'s own `DETAIL_COLUMNS`
— pairs each header with the accessor that fills it, so a column is one declaration rather than
two lists agreeing by convention. `export.rs` renders every one through one `write_sheet` and
contributes only the width arrays, each length-tied to its `COLUMNS.len()`; `report.rs` renders
through one `write_table`; the CSV writes `DETAIL_COLUMNS` through `csv_export::write_csv`. The
drill-down dialog's hand-written labels reuse these spellings ("Device Role", "Pending Patches",
"Aged (past SLA)").
Before this the two had diverged: the report dropped `Patch Type` from the failures table, and
hardcoded the reboot table's headers as "Role"/"Pending patches" against the workbook's "Device
Role"/"Pending Patches".

## The workbook respects Excel's hard limits instead of failing on them

A cell holds at most 32,767 characters and a sheet 1,048,576 rows, and `rust_xlsxwriter` rejects
either overflow with an error that fails the **whole** export. The failures table's `Devices`
cell joined every affected device name, so a patch failing on ~2,000 machines made the workbook
unexportable. That cell is now `rows::join_capped` — as many names as fit, then "… and N more" —
and every other text cell goes through `rows::clamp_cell` as a backstop. Detail rows past one
sheet continue on `Patches (2)`, `Patches (3)`, … with the header and autofilter repeated, rather
than being dropped or failing. The report renders the same capped cell so the two artifacts agree;
the in-app table reads `device_names` whole.

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
rather than inflating `181+ days`; they still count as aged in the SLA rollup (`unwrap_or(true)`
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
predates the window (undated records are kept; the window cannot prove them out).

### An absolute install range replaces the lookback

`FilterParams.installed_after`/`installed_before` (Unix seconds; the To day included whole by the
frontend) let an operator review one past patch cycle. When set they **replace** the relative
lookback rather than intersect it — a 30-day default silently truncating "review March" is the
failure this avoids — and they travel on `FilterParams`, so presets save them. They are validated
by `FilterParams::install_range` (a start is required; start ≤ end; not in the future; span ≤
`MAX_WINDOW_DAYS`), **refused rather than repaired**, and only when an install status is selected:
the Filters panel hides the control otherwise, and a stale range must not fail a Pending query with
its cause off screen. `query_patches` checks before claiming the `QueryToken`, so a malformed range
cannot supersede a good query in flight; `QueryPlan::build` re-checks. Both bounds are pushed down
(`installedAfter`/`installedBefore`, as unspecified in the spec as each other) and both are
re-applied client-side, inclusive, with undated records kept as for the lookback.
