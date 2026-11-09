---
paths:
  - "src-tauri/src/rows/**"
  - "src-tauri/src/export.rs"
  - "src-tauri/src/csv_export.rs"
  - "src-tauri/src/report.rs"
  - "src-tauri/src/history.rs"
  - "src-tauri/src/changes.rs"
---

# Compliance & rollups

Violating these silently misreports a fleet.

- **Every fleet-health rollup uses the `rows::rollup_device` population** (scoped, online,
  `Device::is_patchable`), including the patch loop — pinned by
  `severity_and_age_rollups_cover_the_same_devices_compliance_does`.
  → `docs/design/compliance.md#one-population-for-every-fleet-health-rollup-via-rowsrollup_device`
- **Every surface prints `rows::compliance_scope_note`** (offline + non-patchable counts;
  `devices_total − devices_offline − devices_unpatchable` is the denominator). The frontend
  `util` mirrors it.
  → `docs/design/compliance.md#devices_offline-devices_unpatchable-and-patch_families-ride-on-queryresultquerysummary`
- **Both exports print both clocks, the instance, app version, the result's `sla_policy`, and
  the `QueryScope` facets in two tiers** (`facets` narrow every sheet; `patch_facets` only the
  detail rows), built from the `QueryPlan`, never the request. Date bounds are absolute UTC. The
  CSV states scope + clocks in its file name only.
  → `docs/design/compliance.md#both-exports-state-the-facets-from-rowsqueryscope`
- **Dates are `TableCell::DateTime` (Unix seconds)**: real Excel date-times in UTC; CSV text
  cells are formula-guarded, numbers never. → `docs/design/compliance.md#the-workbook-writes-real-date-times-in-utc`
- **The Devices sheet and the drill-down read one per-device rollup**
  (`build_device_summaries`); an excluded device's counts are blank, not zero.
  → `docs/design/compliance.md#one-per-device-rollup-for-the-devices-sheet-and-the-drill-down`
- **`Type` is a device-tier chip** — rollups cover only the fetched families.
  → `docs/design/compliance.md#the-fleet-health-rollups-do-depend-on-the-patch-type-facet`
- **`is_pending` is an exclude list** (not `REJECTED`/`INSTALLED`); current sources get
  `status_override = MANUAL`; `current_status_set` carries every selected status. The approval
  split (`approval_state`) reads the vendor status, never that override.
  → `docs/design/compliance.md#rowsis_pending-is-an-exclude-list`
- **`Installed` and `Failed` route to the install-history endpoints; current patches are always
  fetched.** One install status and the window (lookback, or an absolute `install_range` that
  replaces it) are pushed down and re-applied client-side.
  → `docs/design/compliance.md#installedfailed-vs-current-patches-status-routing`
- **Changes since last run:** identity is `changes::write_row_key`, scope is tenant +
  `changes::scope_key`, snapshot saved only on `StoreOutcome::Stored`, and awaited before the
  query returns (never detached); the baseline is served from the last snapshot this process
  saved when the scope matches, remembered only once it is on disk; a run identical to it but for
  its time (`RunSnapshot::same_content`) is not rewritten.
  → `docs/design/compliance.md#changes-since-the-previous-comparable-run`
- **Run history is per tenant:** every `RunRecord` carries `history::tenant_key` of the tenant
  label, and `read_run_history` returns only the signed-in tenant's lines (pre-stamp lines by
  instance). Never chart or diff across tenants. Lines are split as bytes before decoding.
  → `docs/design/compliance.md#changes-since-the-previous-comparable-run`
- **SLA aging is per band** (`SlaCutoffs` from the result's `SlaPolicy`); a band with No SLA
  (`SlaExempt`, Optional by default, never `Unknown`) is never past SLA, undated or not.
  → `docs/design/compliance.md#the-sla-is-per-severity-band`
- **Within SLA is the headline** (`DeviceSummary::past_sla == 0`, the worst-devices predicate);
  Fully patched (`pending_count == 0`) beside it; `compliance_total` is device-weighted and
  `uncounted_orgs` names the orgs with no row.
  → `docs/design/compliance.md#within-sla-is-the-headline-fully-patched-is-the-strict-figure-beside-it`
- **Percentages are one decimal everywhere and never round up to 100** (`round_pct` caps at
  99.9; `format_pct`, `pct_cell` and the `web-rs` mirror share the rule).
  → `docs/design/compliance.md#a-percentage-never-rounds-up-to-100`
- **There is no patch release date in the API.** `PatchRecord::first_seen` is detection time; keep
  "First seen" / "since first seen" naming; fixtures must emit `timestamp`.
- **There is no per-patch reboot field.** `PatchRow.needs_reboot` is the device's flag (exported
  "Device Reboot Pending"); `reboot_likely` is `rows::reboot_likely`'s name-based estimate, OS rows
  only, Likely or blank, never No. Both are defined in `rows::REBOOT_COLUMN_NOTES`.
  → `docs/design/compliance.md#there-is-no-per-patch-reboot-field-in-the-ninjaone-api`
- **First seen is the earlier of NinjaOne's time and `crate::first_seen`'s record** (identity
  `changes::write_patch_key`; undated stays undated; merged from the whole fetched feed per family
  at its own fetch time; saved only on `StoreOutcome::Stored`; offline devices carried over).
  → `docs/design/compliance.md#this-app-keeps-its-own-first-seen-record`
  → `docs/design/compliance.md#there-is-no-patch-release-date-in-the-ninjaone-api`
- **`PatchRow` strings are interned `Arc<str>`** (`rows::Interner`, `DeviceLabels`); the
  frontend mirrors them as `String`.
  → `docs/design/compliance.md#patchrow-shares-its-repeated-strings-it-does-not-own-them`
- **Every surface states what a figure means from `rows::wording`** (`headline_figures`,
  `calculation_notes`, `uncounted_orgs_note`, `FirstSeenTracking::sentence`, `PCT_BANDS`); the
  `web-rs` `util` mirrors must reproduce the IPC fixture's `wording` section exactly
  (`the_wording_matches_the_backend`). A new sentence goes in both and in the fixture.
  → `docs/design/compliance.md#every-figure-says-what-it-means-in-the-same-words-everywhere`
- **Tables render through `rows::TableColumn` `COLUMNS`**; the compliance-related Leptos headers
  render from `util::column_help` arrays held to them by `headers_match_the_backend_columns`.
  → `docs/design/compliance.md#table-headers-come-from-rowstablecolumn-spellings`