//! Joins device inventory with patch records to produce the flat per-server patch
//! rows the UI lists and the Excel exporter writes, plus the device rollups that
//! drive the reboot and compliance views.
//!
//! Adapted from `ninjaone-patch-dashboard`'s `snapshot.rs` device↔patch join.
//!
//! Split by concern: `join` (device↔patch join), `compliance` (fleet-health
//! rollups + scope note), `rollups` (failures / severity / age aggregates),
//! `groups` (sort / page / group), `scope` (export provenance), `table` (the
//! shared column definition). This file holds the result types every one of
//! them feeds.

mod compliance;
mod groups;
mod join;
mod rollups;
mod scope;
mod table;

pub use compliance::*;
pub use groups::*;
pub use join::*;
pub use rollups::*;
pub use scope::*;
pub use table::*;

use crate::model::PatchRow;
use serde::Serialize;

/// The full result of a patch query. Cached in `AppState.last_result` and read by
/// the Excel exporter; **not** sent wholesale over IPC — the frontend gets a
/// [`QuerySummary`] and pages the detail rows on demand via `get_patch_rows`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub rows: Vec<PatchRow>,
    pub devices: Vec<DeviceSummary>,
    pub compliance: Vec<ComplianceBucket>,
    /// Compliance grouped by OS name (the Compliance tab's "Compliance by OS").
    pub compliance_by_os: Vec<OsCompliance>,
    /// FAILED-install rollup (empty unless the FAILED status was queried).
    pub failures: Vec<FailureGroup>,
    /// Per-org pending-patch severity breakdown for the dashboard.
    pub severity_by_org: Vec<OrgSeverity>,
    /// Pending-patch age histogram for the dashboard.
    pub age_buckets: Vec<AgeBucket>,
    pub devices_total: usize,
    /// How many of `devices_total` are offline.
    ///
    /// The compliance rollups deliberately exclude offline devices from both halves
    /// of every bucket (they report no current patch records, so a zero pending count
    /// says nothing about them) — which means the `Devices` column of the compliance
    /// table does **not** sum to `devices_total`, and used to have nothing on screen
    /// explaining the gap. Carried so the UI, the workbook and the HTML report can
    /// state the excluded population instead of leaving two different device counts
    /// side by side.
    pub devices_offline: usize,
    /// How many of `devices_total` are online but not something NinjaOne patch
    /// management covers ([`Device::is_patchable`]). Excluded from every fleet-health
    /// rollup alongside the offline devices, and counted separately so the scope
    /// note can name both: `devices_total − devices_offline − devices_unpatchable`
    /// is the compliance denominator. Counted over online devices only so the three
    /// numbers reconcile — an offline switch is in `devices_offline`, not here.
    pub devices_unpatchable: usize,
    /// Which patch families the fleet-health rollups actually cover, taken from the
    /// query's patch-type facet.
    ///
    /// The compliance/severity/age rollups are computed from the *current* patch
    /// feeds, and only the families the query asked for are fetched at all — a
    /// whole-fleet third-party feed is the largest fetch in the app, so an OS-only
    /// query does not page it. That makes "compliant" mean "no pending OS patches"
    /// on such a query, which is a defensible reading but not one the operator can
    /// infer from a bare percentage. Reported so every surface can name its scope.
    pub patch_families: PatchFamilies,
    /// The facets this result was computed under, for the exports' provenance block.
    /// `QueryResult`-only on purpose — see [`QueryScope`].
    pub scope: QueryScope,
    /// When the query was computed (the join/rollup clock).
    pub generated_at: String,
    /// When the underlying whole-fleet patch data was last fetched from NinjaOne —
    /// distinct from `generated_at` because a re-filter recomputes over the cached
    /// fetch without a new round trip. Drives the UI's "patch data as of …" label.
    pub data_fetched_at: String,
}

/// The lightweight view of a query returned to the frontend over IPC: the first
/// page of detail rows plus the rollups (compliance, the reboot subset, totals).
/// The remaining detail rows stay in the backend cache and are fetched a page at a
/// time, so a 10k+ row fleet doesn't serialize multiple MB of JSON into the WASM
/// webview on every query.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QuerySummary {
    /// The first page of detail rows; later pages come from `get_patch_rows`.
    pub rows: Vec<PatchRow>,
    /// Total detail-row count (the table pages over this, not `rows.len()`).
    pub rows_total: usize,
    /// Only the devices flagged for reboot — all the reboot view renders. The full
    /// device list stays in the cache for export.
    pub reboot_devices: Vec<DeviceSummary>,
    pub compliance: Vec<ComplianceBucket>,
    /// Compliance grouped by OS name (the Compliance tab's "Compliance by OS").
    pub compliance_by_os: Vec<OsCompliance>,
    /// FAILED-install rollup — small (one entry per failing patch), so it ships
    /// whole rather than paged like the detail rows.
    pub failures: Vec<FailureGroup>,
    /// Per-org pending-patch severity breakdown for the dashboard charts.
    pub severity_by_org: Vec<OrgSeverity>,
    /// Pending-patch age histogram for the dashboard charts.
    pub age_buckets: Vec<AgeBucket>,
    pub devices_total: usize,
    /// How many of `devices_total` are offline.
    ///
    /// The compliance rollups deliberately exclude offline devices from both halves
    /// of every bucket (they report no current patch records, so a zero pending count
    /// says nothing about them) — which means the `Devices` column of the compliance
    /// table does **not** sum to `devices_total`, and used to have nothing on screen
    /// explaining the gap. Carried so the UI, the workbook and the HTML report can
    /// state the excluded population instead of leaving two different device counts
    /// side by side.
    pub devices_offline: usize,
    /// How many of `devices_total` are online but not something NinjaOne patch
    /// management covers ([`Device::is_patchable`]). Excluded from every fleet-health
    /// rollup alongside the offline devices, and counted separately so the scope
    /// note can name both: `devices_total − devices_offline − devices_unpatchable`
    /// is the compliance denominator. Counted over online devices only so the three
    /// numbers reconcile — an offline switch is in `devices_offline`, not here.
    pub devices_unpatchable: usize,
    /// Which patch families the fleet-health rollups actually cover, taken from the
    /// query's patch-type facet.
    ///
    /// The compliance/severity/age rollups are computed from the *current* patch
    /// feeds, and only the families the query asked for are fetched at all — a
    /// whole-fleet third-party feed is the largest fetch in the app, so an OS-only
    /// query does not page it. That makes "compliant" mean "no pending OS patches"
    /// on such a query, which is a defensible reading but not one the operator can
    /// infer from a bare percentage. Reported so every surface can name its scope.
    pub patch_families: PatchFamilies,
    pub generated_at: String,
    /// When the underlying whole-fleet patch data was last fetched (see
    /// [`QueryResult::data_fetched_at`]).
    pub data_fetched_at: String,
}

impl QuerySummary {
    /// Builds the IPC summary from the full result, cloning only the first
    /// `first_page` rows and the reboot subset (not the whole row/device sets).
    pub fn from_result(result: &QueryResult, first_page: usize) -> Self {
        Self {
            rows: result.rows.iter().take(first_page).cloned().collect(),
            rows_total: result.rows.len(),
            reboot_devices: result
                .devices
                .iter()
                .filter(|d| d.needs_reboot)
                .cloned()
                .collect(),
            compliance: result.compliance.clone(),
            compliance_by_os: result.compliance_by_os.clone(),
            failures: result.failures.clone(),
            severity_by_org: result.severity_by_org.clone(),
            age_buckets: result.age_buckets.clone(),
            devices_total: result.devices_total,
            devices_offline: result.devices_offline,
            devices_unpatchable: result.devices_unpatchable,
            patch_families: result.patch_families,
            generated_at: result.generated_at.clone(),
            data_fetched_at: result.data_fetched_at.clone(),
        }
    }
}

#[cfg(test)]
mod tests;
