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
because whatever current records NinjaOne still holds for them were collected before they went
offline, so neither a pending count nor a zero says anything current about them (the
offline-backlog list shows what is there); switches, printers, hypervisors and cloud monitors are
excluded because they carry no patch records at all and would otherwise score *compliant*, so 100 servers plus 100
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
severity band, urgent pending past SLA (`urgent_past_sla`), failed installs and NinjaOne's `lastContact` —
filled by `rows::build_device_summaries` from the **same unnarrowed current feed** and the same
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
  (indexed by `rank()`, unique per variant) and `is_aged` reads the record's own band. Undated
  records still count as aged — unless the band has No SLA (below).
- **A band can be held to no SLA at all** (`Settings.sla_exempt`, `SlaExempt`; the Settings
  panel's "No SLA" box beside each band). `SlaPolicy::window_for` returns `None` for it and
  `SlaCutoffs` has no cutoff, so `is_aged` is false — undated or not, since there is no target to
  have missed. Its records are still *pending*, so they still break Fully patched; only the
  past-SLA figures ignore them. **Optional is on by default**, and `SlaExempt` is its own struct
  rather than a sentinel inside `SlaBySeverity` for that reason: `Settings::save` writes every
  override band, so files on disk already hold `"optional": null`, and a sentinel could not tell
  "never set" from "cleared" — existing installs would never have picked up the default. Its
  struct-level `#[serde(default)]` keeps the default for every band a partial block omits.
  `Unknown` has no switch: an unrated patch cannot be shown to be unimportant. `describe()` names
  the exempt bands in a closing `no SLA: …` clause and drops any override they kept. Stuck
  approvals ignore the switch (see below).
- **Two "past SLA" figures, on purpose.** The compliance tables' "Urgent Past SLA" stays the
  urgent subset of the column beside it ("Urgent Pending" — Critical, Important or Security,
  `Severity::is_urgent`), so the two still reconcile. The worst-devices and offline lists' "Past SLA" counts *every* pending record
  against its own band's window — that is what the overrides for Moderate/Low are for, and why
  Optional defaults to No SLA.
- **The result carries the policy it was computed with** (`QueryResult`/`QuerySummary`
  `sla_policy`). An export after a settings change states the numbers' policy, not the new one,
  and the Compliance tab prints it for the same reason.
- **Changing it clears no cache.** The policy is read from the settings snapshot when a query is
  assembled; only an instance/client-id change invalidates fleet data. The next **Run query** — a
  re-filter over the warm cache, no refetch — reflects the new policy; the result on screen keeps
  its own until then.
- The run history's `urgent_past_sla` is recorded under whatever policy each run used, so a trend
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
  yet still appear in NinjaOne's current feed with pending records. Their records are why they
  are excluded from the rollups — collected before the device went offline, so nothing about them
  is current — but dropping them everywhere made that backlog invisible. The records may be stale; every surface
  says so.
- **Two different clocks, both shown.** "Latest Patch Data Collected" (`latest_collected`) is
  the newest of NinjaOne's own `timestamp`s on the device's pending records — how current the
  list is for it — and always reads NinjaOne's time, never the first-seen ledger's. "Last
  Contact" is `Device::last_contact`, when its agent last checked in — how long it has been gone.
  The offline exclusion stays (see above); these two let a reader judge how far to trust each
  row.

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

- `QueryResult`-only, deliberately **not** on `QuerySummary`: the frontend keeps its own snapshot
  of the applied filters (`AppliedFilters`),
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

Patch identity is `changes::patch_key` (built from a raw record by `changes::write_patch_key`,
which the first-seen ledger uses, so the two cannot disagree): an OS patch is its normalized KB, a
third-party patch its **product** (`productIdentifier`, shared by every version), anything else
its lowercased title. Keyed by title, a superseding version ("Chrome …55" → "…66") was a new patch:
the diff listed it as resolved-and-new, and its age restarted, so a device that never updated
Chrome never went past SLA. `SNAPSHOT_VERSION` 2 marks the change; a version-1 baseline reads as
no previous run. OS cumulative updates still restart with each new KB — the API carries no
product-line identity for them.

The baseline advances only when a result **wins the cache** (`StoreOutcome::Stored`); a
superseded run is older than the one that won, and writing it would roll the baseline back. The
write is atomic (temp + rename, 0600) on a blocking thread, and `query_patches` awaits it before
returning (`persist_if_stored`). It used to be detached, so a query started right after
(an auto-refresh tick, a quick re-run) could load the baseline before the save landed and diff
against the run before last. The snapshot that write landed is also kept in memory
(`changes::LastSaved`, one slot) as the next baseline for the same tenant and scope, so a refresh
cadence over one scope never re-reads and re-parses the file; another scope reads its file. Only a
snapshot that reached disk is remembered, so memory never names a baseline the next launch cannot
find. The directory keeps the 20 most
recently written scopes within 64 MB, always keeping the newest. A run past a million items is
not written and says so (`too_large`). Consequence worth knowing: every stored run is the next
baseline, so re-running the same scope over a warm cache reports "no changes since" the run
seconds earlier — "since the previous run", literally.

**The run history is versioned by calculation method** (`history::METHOD`, stored per line as
`method`; lines from before it read as 0). `RunRecord::comparable_with` in `web-rs` requires the
same method as well as the same scope (`same_scope`), so a change in what a figure *means* — Within
SLA replacing zero-pending, Security joining the urgent backlog, No SLA, the first-seen ledger —
never charts as the fleet moving; the Trend tab says how many runs of the scope it left out
(`util::older_method_note`). A new rule that changes a recorded figure bumps `METHOD`. Within SLA
and the fleet's pending count (`pending_total`, from `severity_by_org`) are recorded from method
1: the "Pending patches" card used to plot `rows_total`, the detail-row count after every patch
filter, which is not the backlog. The urgent fields keep their original on-disk keys
(`pendingCritical`/`agedCritical`, `pc`/`ac`).

The Trend tab's per-organization table reads `history::RunRecord::orgs` (the Compliance tab's
per-org rows plus each org's pending count), capped at the 100 largest organizations per line so
an MSP's history file stays bounded; `read_run_history` returns that detail only for the newest
200 lines, since the table compares only the newest run with the previous comparable one. Cards
lead with the change against that previous run; the change over the whole series is the footer.

## Awaiting approval and approved-not-installed are read from the vendor status

Both compliance rollups carry `awaiting_approval` (vendor status `MANUAL`, NinjaOne's "Pending") and
`approved_not_installed` (`APPROVED`), over the same `rollup_device` population and across **every**
severity — the split describes the workflow, not the urgency, unlike the urgent SLA
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
only let the two disagree. It is the *default* window for every band, and it ignores No SLA: an
approval is an explicit decision to install, so an approved Optional patch that never installs is
exactly the agent symptom this list exists for. An undated approved patch counts as stuck, the same rule `is_aged` uses.
The workbook writes every stuck device on a **Stuck Approvals** sheet with the threshold as a
footnote; the report and the Compliance tab show the oldest ones and say how many more there are.

## Within SLA is the headline; Fully patched is the strict figure beside it

Every compliance table carries two device counts over the same `rollup_device` population:

- **Within SLA** — `DeviceSummary::past_sla == 0`: no pending record, of any band, first seen
  longer ago than its band's window (`SlaCutoffs::is_aged`; a No-SLA band never counts). This
  is the headline: it measures the fleet against the policy the operator set. "Zero pending"
  alone failed a device for an Optional patch nobody intends to approve and for a Critical
  found an hour ago, so on a real Windows fleet it hovered near zero and moved for reasons that
  were not a patching backlog.
- **Fully patched** — `pending_count == 0` over *any* severity and age (`is_pending`); the old
  "Compliant". Kept because "nothing outstanding at all" is still a question worth answering, and
  because Fully patched ≤ Within SLA always, so the gap is the in-window backlog.

`past_sla` is the very count the worst-devices list ranks by (`DeviceBacklog::past_sla`, the same
`is_aged` over the same records), so a device is Within SLA exactly when it would show Past SLA
0 there — `within_sla_counts_devices_with_nothing_past_its_window` pins it.

The two urgent columns grade differently again: they count only Critical, Important and Security
records (`counts_toward_backlog`, `Severity::is_urgent`). A row can legitimately read "10 devices ·
9 within SLA · 4 fully patched · 0 urgent pending".

### The fleet total is device-weighted

`QueryResult.compliance_total` (`ComplianceBucket::total`, labelled "All organizations") sums the
organizations' counts and recomputes both percentages from the sums — never the mean of the
rows, which would let a 2-device org weigh as much as a 2,000-device one. It is exact because both
halves of `accumulate_compliance` key by the same org-name lookup, so every counted device and
record lands in one organization. An empty scope's total is 100% by the bucket rule, so every
surface prints "—" when `devices_total` is 0. The run history reads its fleet figures from it.

### Organizations with no counted device are named

An organization whose devices are all offline or non-patchable gets no bucket (a bucket with no
devices once read "0 devices · 100% compliant"), so it used to vanish from the table without a
word. `QueryResult.uncounted_orgs` (`rows::uncounted_orgs`) names them, and the tab prints
`util::uncounted_orgs_note` under the table (capped at ten names).

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

Every percentage is one decimal on every surface — `rows::format_pct` and `pct_cell` share
`round_pct`, the report prints number cells `{:.1}`, and the `web-rs` `format_pct` mirrors it (the
tab once showed 0 decimals beside the Trend tab's 1). Anything below 100 caps at 99.9. Plain
rounding prints "100.0%" from 99.95% up, so 1,999 of 2,000 devices patched reads as a clean fleet —
the one rounding error here that changes what an operator does.

## Enumerate bands through an accessor list, never by matching a label string

`rows::SeverityCounts::BANDS` and `charts::SEV_BANDS` both pair each band with the function that
reads it, and their totals derive from that list. A version that matched on the display label
with a `_ => c.unknown` fallback let a renamed band silently draw Unknown's count twice and
overflow the bar. See [severity.md](./severity.md).

## Every figure says what it means, in the same words everywhere

A bare "Compliance %" had no definition anywhere in the app (only in `docs/TROUBLESHOOTING.md`,
and narrower than the code), so the GUI and the exports now say what each figure is, from one
set of sentences in `rows::wording`:

- **Headline figures** (`headline_figures`): Within SLA, Fully patched, Urgent pending, Excluded
  devices — value, "n of N", one-line definition. The Compliance tab's KPI tiles, the report's
  header line, the workbook's About **Headline** block. Over no counted device a percentage is
  "—", never the empty bucket's 100%.
- **Definitions** (`calculation_notes`): every term the tab uses, built from the result's own SLA
  policy, patch families and first-seen record. The tab's collapsed "How these numbers are
  calculated", the report's closing section, the About **Definitions** block (the compliance
  sheets point to it).
- **Column help**: every compliance, device-list, stuck-approvals, time-to-install and Trend
  header, and the drill-down's health labels, render from `util::column_help`'s per-table
  `(header, definition)` arrays through `HelpHeaderRow` — a `title=` tooltip with a dotted
  underline. Per table, because "Approved, Not Installed" counts every approved patch in one table
  and only the stuck ones in another. The workbook gets no per-column notes; its Definitions block
  covers them.
- **The legend**: `PCT_BANDS` (95% or more, 80% to under 95%, under 80%) colours the Within SLA
  bars in the app and the report, and each draws the key beneath them.

**The crates share no code, so the fixture pins the words.** `ipc_fixture_is_current` writes this
module's output for the fixture's own result under `wording` — headline, definitions, first-seen
line, missing-orgs note, SLA policy, scope note, both device-list notes, legend labels — and the
backend's `COLUMNS` header lists under `wording.columns`. `the_wording_matches_the_backend`
requires the `util` mirrors to reproduce every string exactly, and
`headers_match_the_backend_columns` holds the hand-spelled headers to `COLUMNS` (replacing
"spelled identically by review", which had already let the offline note's "Latest data" drift from
its own column). `every_header_is_defined_or_an_identity_column` fails a measuring header with no
definition.

## Table headers come from `rows::TableColumn` spellings

The Leptos tables are hand-written and are not wired to `COLUMNS`; the compliance ones are held
to them by `headers_match_the_backend_columns` (above), the rest by review: "Within SLA", "Within SLA %", "Fully Patched", "Fully Patched %",
"Urgent Pending", "Urgent Past SLA",
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
"Urgent Past SLA").
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
`Patch::collected_timestamp` (alias `timestamp`, read via `collected_at()` into
`rows::PatchRecord::first_seen`) is therefore
**detection time, not publication time**, and everything derived from it —
`PatchRow.first_seen_ts`/`first_seen_date`, the SLA `urgent_past_sla` rollup, `build_age_buckets`,
and the `detected_within_days`/`detected_after`/`detected_before` filter window — measures *how
long we have known about the patch*. The UI says so ("First seen", "Pending past SLA", "Pending
patch age (since first seen)"); keep the naming honest if you touch these.

This was once a field named `release_timestamp` aliasing a `releaseDate` that never binds, so the
SLA rollup compared *now* against an always-recent timestamp and reported ~0 breaches on any
fleet — and the wiremock fixtures fed `releaseDate`, so CI proved only that the aliasing worked.
**Fixtures must emit `timestamp`.** Undated pending patches get their own `Unknown` age bucket
rather than inflating `181+ days`; they still count as aged in the SLA rollup (`unwrap_or(true)`
— can't prove recent).

## This app keeps its own first-seen record

`timestamp` is "Date/Time when data was collected/updated", and nothing in the spec says whether
a rescan refreshes it. If it does, every pending record looks a day old and nothing is ever past
SLA — the same ~0-breaches failure the old `releaseDate` alias produced, with nothing on screen
to show it. Rather than bet the headline figure on which kind of time the vendor sends,
`crate::first_seen` keeps a per-tenant record of the fetch at which each pending
`(device, patch)` first appeared in the current feed, and every rollup ages a record from
`rows::PatchRecord::first_seen` = **the earlier of the two** (`PatchRecord::observed`). When
NinjaOne's time is the older one nothing changes; when it is not, this record is. Rules:

- **One identity:** `changes::write_patch_key`, the changes diff's own key — a third-party patch
  by product, so a superseding version keeps its age.
- **Undated stays undated.** A record NinjaOne never dated already counts as past SLA; dating it
  from this record would only make it younger. So the record can only make a record older, never
  newer, and never lowers a past-SLA count.
- **Merged from the whole fetched feed, per family, at that family's own fetch time**
  (`CurrentPatches::{os,sw}_fetched_at`; the combined `fetched_at` is the older family's). An
  org-scoped query sees every org's records (the feed is whole-fleet), so it can stamp them and
  can never prune them for being out of view. A family the query did not fetch is untouched.
- **Pruned when no longer pending**, except for devices offline in the inventory — their absence
  proves nothing, the `rollup_device` reasoning — so a deleted device's records fall out and a
  sleeping laptop's do not. A feed no newer than the last one merged (`merged_at`) is skipped, and
  a feed with no pending record never prunes a non-empty record.
- **Persisted like the run snapshots:** one file per tenant under `first-seen/` (tenant stamped
  inside and checked on load), atomic temp + rename, 0600, written only on
  `StoreOutcome::Stored` and awaited (`persist_if_stored`), the last-written copy held in memory
  (`SLOT`) as the next base. Capped at a million entries per family (the newest dropped first)
  and 8 tenants / 128 MiB. Unreferenced keys are not written.
- **It only knows what this install has seen.** It starts at the first query after the upgrade,
  is per machine (two operators keep two records), and a patch that disappears and returns starts
  over. Every surface that defines "first seen" says so: `QueryResult.first_seen`
  (`rows::FirstSeenTracking`) carries since when the record has been kept and how many of the
  counted pending records it dated — a count that climbs run after run *is* the evidence that
  NinjaOne's times reset. Install-history records keep NinjaOne's time; time to install is not
  re-dated.

## `PatchRow` shares its repeated strings; it does not own them

Device/org/location/role/OS names, patch titles, KBs and statuses are `Arc<str>` handed out by a
per-join interner (`rows::Interner`), the device-derived half is resolved once per device
(`rows::DeviceLabels`) rather than once per patch, and `patch_type`/`severity` are `&'static str`
because both vocabularies are fixed. The cached `QueryResult` is the app's largest live
allocation, and it once held one owned `String` per field per row for a few thousand distinct
values. `FailureGroup` and `PatchGroup` carry the same shared strings so the rollups are refcount
bumps. All of it serializes to plain JSON strings, so `web-rs/src/types.rs` still mirrors them as
`String` — and because the two crates share no code, the IPC fixture (`web-rs/tests/backend-ipc.json`)
decodes the backend's real rows through those mirrors, so a field that stopped serializing as a
string fails `just web-test`.

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
the patch filter bar hides the control otherwise, and a stale range must not fail a Pending query
with its cause off screen. `query_patches` checks before claiming the `QueryToken`, so a malformed range
cannot supersede a good query in flight; `QueryPlan::build` re-checks. Both bounds are pushed down
(`installedAfter`/`installedBefore`, as unspecified in the spec as each other) and both are
re-applied client-side, inclusive, with undated records kept as for the lookback.
