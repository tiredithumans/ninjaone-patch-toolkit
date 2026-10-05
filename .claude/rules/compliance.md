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
  (`apply_device_health`); an excluded device's counts are blank, not zero.
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
- **Changes since last run:** identity is `changes::patch_key`, scope is tenant +
  `changes::scope_key`, snapshot saved only on `StoreOutcome::Stored`, and awaited before the
  query returns (never detached).
  → `docs/design/compliance.md#changes-since-the-previous-comparable-run`
- **SLA aging is per band** (`SlaCutoffs` from the result's `SlaPolicy`).
  → `docs/design/compliance.md#the-sla-is-per-severity-band`
- **`format_pct` never rounds up to 100** (caps at 99%; `pct_cell` at one decimal).
  → `docs/design/compliance.md#a-percentage-never-rounds-up-to-100`
- **There is no patch release date in the API.** `first_seen_at()` is detection time; keep
  "First seen" / "since first seen" naming; fixtures must emit `timestamp`.
  → `docs/design/compliance.md#there-is-no-patch-release-date-in-the-ninjaone-api`
- **`PatchRow` strings are interned `Arc<str>`** (`rows::Interner`, `DeviceLabels`); the
  frontend mirrors them as `String`.
  → `docs/design/compliance.md#patchrow-shares-its-repeated-strings-it-does-not-own-them`
- **Tables render through `rows::TableColumn` `COLUMNS`**; the hand-written Leptos headers match
  those spellings by review.
  → `docs/design/compliance.md#table-headers-come-from-rowstablecolumn-spellings`