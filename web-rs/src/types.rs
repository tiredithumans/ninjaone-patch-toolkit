//! DTOs mirroring the Tauri backend. Serialized field names use camelCase to match
//! the backend's serde contract across the IPC boundary.
//!
//! These types are a hand-maintained mirror of the backend arg/result structs
//! (`src-tauri/src/{rows,model,commands}.rs`). A backend test,
//! `serialized_shapes_carry_every_frontend_required_key` in `src-tauri/src/rows.rs`,
//! fails if the backend drops/renames a key the mirrors below read — so drift is
//! caught in CI rather than as a silently blank column at runtime.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterParams {
    pub organization_id: Option<i64>,
    pub location_id: Option<i64>,
    pub role_id: Option<i64>,
    pub node_classes: Vec<String>,
    pub os_name_contains: Option<String>,
    pub search: Option<String>,
    /// Patch severities to keep (e.g. `CRITICAL`); empty = all.
    #[serde(default)]
    pub severities: Vec<String>,
    /// Release-date filter: relative window (last N days) and/or absolute bounds
    /// (Unix seconds) for a custom range.
    #[serde(default)]
    pub release_within_days: Option<i64>,
    #[serde(default)]
    pub release_after: Option<i64>,
    #[serde(default)]
    pub release_before: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Organization {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Location {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Role {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeClass {
    pub value: String,
    pub label: String,
}

// Backend also sends device_id, node_class, severity_rank and raw timestamps;
// serde ignores fields not declared here. Only what the table renders is kept.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchRow {
    pub device_name: String,
    pub organization: String,
    pub location: Option<String>,
    pub device_role: Option<String>,
    pub os_name: Option<String>,
    pub patch_type: String,
    pub kb: Option<String>,
    pub name: String,
    pub severity: String,
    pub status: String,
    pub release_date: Option<String>,
    pub installed_date: Option<String>,
}

/// A device row for the Needs-Reboot view. The backend only sends the
/// reboot-needing subset, so this mirror omits the `needsReboot` flag (always true
/// here) — extra fields in the JSON are ignored on deserialize.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub device_name: String,
    pub organization: String,
    pub location: Option<String>,
    pub device_role: Option<String>,
    pub os_name: Option<String>,
    pub pending_count: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceBucket {
    pub organization: String,
    pub devices_total: usize,
    pub devices_compliant: usize,
    pub compliance_pct: f64,
    pub pending_critical: usize,
    pub aged_critical: usize,
}

// Backend also sends patchType, severityRank and latestFailureTs; serde ignores
// undeclared fields. Only what the failures table renders is mirrored here.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureGroup {
    pub kb: Option<String>,
    pub name: String,
    pub severity: String,
    pub affected_devices: usize,
    pub sample_devices: Vec<String>,
    pub latest_failure: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityCounts {
    pub critical: usize,
    pub important: usize,
    pub moderate: usize,
    pub low: usize,
    pub optional: usize,
    pub unknown: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgSeverity {
    pub organization: String,
    pub counts: SeverityCounts,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgeBucket {
    pub label: String,
    pub count: usize,
}

/// The summary the backend returns from `query_patches`: the first page of detail
/// rows plus the rollups. The remaining detail rows stay in the backend cache and
/// are fetched a page at a time via `get_patch_rows`, so a large fleet doesn't ship
/// every row over IPC. Mirrors the backend `QuerySummary`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    /// First page of detail rows; seeds the table without an extra round trip.
    pub rows: Vec<PatchRow>,
    /// Total detail-row count — the table pages over this, not `rows.len()`.
    pub rows_total: usize,
    /// Only the devices flagged for reboot (all the reboot view needs).
    pub reboot_devices: Vec<DeviceSummary>,
    pub compliance: Vec<ComplianceBucket>,
    /// FAILED-install rollup (empty unless the FAILED status was queried).
    pub failures: Vec<FailureGroup>,
    /// Per-org pending-patch severity breakdown for the dashboard charts.
    pub severity_by_org: Vec<OrgSeverity>,
    /// Pending-patch age histogram for the dashboard charts.
    pub age_buckets: Vec<AgeBucket>,
    pub devices_total: usize,
    pub generated_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub authenticated: bool,
    pub instance_base_url: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub instance_base_url: String,
    pub client_id: Option<String>,
    pub callback_port: u16,
    pub install_window_days: i64,
    pub sla_days: i64,
    pub has_client_secret: bool,
    pub presets: Vec<Preset>,
    pub auto_check_updates: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    pub filter: FilterParams,
    /// Patch-query selectors restored alongside the filter facets. Optional for
    /// backward compatibility with presets saved before they were captured.
    #[serde(default)]
    pub patch_type: Option<String>,
    #[serde(default)]
    pub statuses: Option<Vec<String>>,
    #[serde(default)]
    pub install_days: Option<i64>,
}

/// Incremental progress for an in-flight `query_patches`, delivered on the
/// `query:progress` event. `stage` is one of `devices` / `osPatches` /
/// `swPatches` / `osInstalls` / `swInstalls` / `joining`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryProgressEvent {
    pub query_id: u64,
    pub stage: String,
    pub loaded: usize,
}

/// Available-update metadata from the backend updater. `notes` is the published
/// release body (the changelog) shown in the update splash.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    pub current_version: String,
    pub notes: Option<String>,
}

// --- Command argument payloads (frontend → backend) --------------------------

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchQueryArgs {
    pub filter: FilterParams,
    /// "ALL" | "OS" | "SOFTWARE"
    pub patch_type: String,
    /// "PENDING" | "APPROVED" | "REJECTED" | "INSTALLED" | "FAILED"
    pub statuses: Vec<String>,
    pub install_after_days: Option<i64>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SaveSettingsArgs {
    pub instance_base_url: String,
    pub client_id: Option<String>,
    pub callback_port: u16,
    pub install_window_days: i64,
    pub sla_days: i64,
    pub client_secret: Option<String>,
    pub clear_secret: bool,
    pub auto_check_updates: bool,
}
