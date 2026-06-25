//! DTOs mirroring the Tauri backend. Serialized field names use camelCase to match
//! the backend's serde contract across the IPC boundary.

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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub device_name: String,
    pub organization: String,
    pub location: Option<String>,
    pub device_role: Option<String>,
    pub os_name: Option<String>,
    pub needs_reboot: bool,
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

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub rows: Vec<PatchRow>,
    pub devices: Vec<DeviceSummary>,
    pub compliance: Vec<ComplianceBucket>,
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
