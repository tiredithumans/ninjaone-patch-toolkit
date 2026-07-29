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

// Backend also sends node_class, severity_rank and raw timestamps; serde ignores
// fields not declared here. `device_id` IS mirrored — it is the row's identity for
// action selection, since NinjaOne has no per-patch apply endpoint and checking a
// row therefore selects that row's *device*.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchRow {
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub location: Option<String>,
    pub device_role: Option<String>,
    pub os_name: Option<String>,
    /// NinjaOne queues an action for an offline device rather than rejecting it, so
    /// the selection surfaces this to explain why a target may be skipped.
    pub offline: bool,
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

/// Per-OS compliance row for the Compliance tab's "Compliance by OS" section.
/// Mirrors the backend `OsCompliance`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OsCompliance {
    pub os: String,
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
    /// Every affected device name (the full list, not a sample).
    pub device_names: Vec<String>,
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
    /// Compliance grouped by OS name (the "Compliance by OS" section).
    pub compliance_by_os: Vec<OsCompliance>,
    /// FAILED-install rollup (empty unless the FAILED status was queried).
    pub failures: Vec<FailureGroup>,
    /// Per-org pending-patch severity breakdown for the dashboard charts.
    pub severity_by_org: Vec<OrgSeverity>,
    /// Pending-patch age histogram for the dashboard charts.
    pub age_buckets: Vec<AgeBucket>,
    pub devices_total: usize,
    pub generated_at: String,
    /// When the underlying whole-fleet patch data was last fetched (vs. when this
    /// re-filter was computed). Drives the "patch data as of …" label.
    pub data_fetched_at: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    pub authenticated: bool,
    pub instance_base_url: String,
    /// Patch actions switched on in Settings.
    #[serde(default)]
    pub actions_enabled: bool,
    /// The current grant carries `management`, so the write endpoints will accept
    /// us. False while `actions_enabled` is true means re-authorization is needed.
    #[serde(default)]
    pub write_enabled: bool,
    /// Whether the grant's scope could be read at all. False here makes
    /// `write_enabled == false` mean "unknown" rather than "denied".
    #[serde(default)]
    pub scope_known: bool,
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
    #[serde(default)]
    pub actions: ActionSettings,
}

/// Mirror of the backend `settings::ActionSettings`. Round-trips unchanged through
/// the settings panel, so a field the UI doesn't expose is preserved rather than
/// reset to its default on save.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ActionSettings {
    pub enabled: bool,
    pub os_patch_script_id: Option<i64>,
    pub software_patch_script_id: Option<i64>,
    pub run_as: String,
    pub concurrency: usize,
    pub max_devices_per_action: usize,
    pub max_orgs_per_action: usize,
    pub allow_offline_targets: bool,
    pub require_maintenance_window: bool,
    pub window_days: Vec<u8>,
    pub window_start_minute: u16,
    pub window_end_minute: u16,
    pub allow_window_override: bool,
}

impl Default for ActionSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            os_patch_script_id: None,
            software_patch_script_id: None,
            run_as: "system".into(),
            concurrency: 8,
            max_devices_per_action: 25,
            max_orgs_per_action: 1,
            allow_offline_targets: false,
            require_maintenance_window: false,
            window_days: vec![1, 2, 3, 4, 5],
            window_start_minute: 120,
            window_end_minute: 300,
            allow_window_override: false,
        }
    }
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

/// Sort request for the paged Patches table (`get_patch_rows`). Mirrors the
/// backend `rows::{RowSort, RowSortKey}`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RowSort {
    pub key: RowSortKey,
    pub desc: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RowSortKey {
    Organization,
    Location,
    Role,
    Device,
    Os,
    PatchType,
    Kb,
    Name,
    Severity,
    Status,
    ReleaseDate,
    InstalledDate,
}

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
    pub actions: ActionSettings,
}

// --- Device actions ----------------------------------------------------------

/// Mirror of the backend `actions::ActionKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ActionKind {
    OsPatchScan,
    SoftwarePatchScan,
    OsPatchApply,
    SoftwarePatchApply,
    Reboot,
    Script,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OsPatchScan => "Scan for OS patches",
            Self::SoftwarePatchScan => "Scan for software patches",
            Self::OsPatchApply => "Apply OS patches",
            Self::SoftwarePatchApply => "Apply software patches",
            Self::Reboot => "Reboot",
            Self::Script => "Run script",
        }
    }

    /// Mirrors the backend rule: scans don't change the device, so they need no
    /// confirmation. Display-only here — the backend enforces it for real.
    pub fn is_mutating(self) -> bool {
        !matches!(self, Self::OsPatchScan | Self::SoftwarePatchScan)
    }

    /// Whether this action can restart the device as a side effect.
    pub fn can_reboot(self) -> bool {
        matches!(
            self,
            Self::Reboot | Self::OsPatchApply | Self::SoftwarePatchApply | Self::Script
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RebootMode {
    Normal,
    Forced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RebootChoice {
    #[default]
    Never,
    Auto,
}

/// Mirror of the backend `actions::JobState`, which serializes as an internally
/// tagged `{ state, detail }`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", tag = "state", content = "detail")]
pub enum JobState {
    Queued,
    Running,
    Completed,
    Failed(String),
    TimedOut,
    Unknown(String),
    Skipped(String),
}

impl JobState {
    pub fn label(&self) -> String {
        match self {
            Self::Queued => "Queued".into(),
            Self::Running => "Running".into(),
            Self::Completed => "Completed".into(),
            Self::Failed(msg) => format!("Failed: {msg}"),
            Self::TimedOut => "Timed out".into(),
            Self::Unknown(msg) => format!("Unknown: {msg}"),
            Self::Skipped(why) => format!("Skipped: {why}"),
        }
    }

    /// CSS modifier for the status pill.
    pub fn css_class(&self) -> &'static str {
        match self {
            Self::Completed => "job-state job-state-ok",
            Self::Failed(_) | Self::TimedOut => "job-state job-state-bad",
            Self::Unknown(_) => "job-state job-state-warn",
            Self::Skipped(_) => "job-state job-state-muted",
            Self::Queued | Self::Running => "job-state job-state-running",
        }
    }
}

// The backend also sends batchId, deviceId, dispatchedTs and finishedAt; serde
// ignores fields not declared here. The correlators (`activityId`/`seriesUid`) ARE
// kept: NinjaOne v2 has no script-output endpoint, so they are how an operator
// finds the run in the NinjaOne console.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobReport {
    pub id: u64,
    pub device_name: String,
    pub organization: String,
    pub kind: ActionKind,
    pub detail: String,
    pub dry_run: bool,
    pub state: JobState,
    pub dispatched_at: String,
    pub duration_seconds: Option<i64>,
    pub activity_id: Option<i64>,
    pub series_uid: Option<String>,
    pub exit_code: Option<i32>,
}

impl JobReport {
    /// Where to look this run up in NinjaOne, shown as a tooltip on the status
    /// cell. Empty when the tenant's dispatch response carried no correlator.
    pub fn correlator(&self) -> String {
        match (self.activity_id, self.series_uid.as_deref()) {
            (Some(id), _) => format!("NinjaOne activity {id}"),
            (None, Some(uid)) => format!("NinjaOne job {uid}"),
            _ => String::new(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedTarget {
    pub device_name: String,
    pub organization: String,
    pub offline: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedTarget {
    pub device_name: String,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionPlan {
    pub summary: String,
    pub eligible: Vec<PlannedTarget>,
    pub skipped: Vec<SkippedTarget>,
    pub organizations: Vec<String>,
    pub warnings: Vec<String>,
    pub blockers: Vec<String>,
    pub reboot_expected: bool,
    pub dry_run: bool,
    pub parameters_preview: Option<String>,
    /// Absent when the plan is blocked — there is nothing to confirm.
    pub confirm_token: Option<String>,
}

impl ActionPlan {
    pub fn is_blocked(&self) -> bool {
        !self.blockers.is_empty()
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionBatch {
    pub dispatched: usize,
    pub skipped: usize,
    /// The rows just created, used to seed the Jobs tab without a second round
    /// trip. The backend poller then advances them over `action:progress`.
    pub jobs: Vec<JobReport>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptSummary {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub operating_systems: Vec<String>,
    /// Whether the library entry declares a `kbAllowList` variable. Only such a
    /// script may be offered per-KB targeting — anything else installs whatever the
    /// device needs, and offering it would misrepresent what runs.
    pub accepts_kb_allow_list: bool,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAsOptions {
    pub roles: Vec<String>,
}

/// Mirror of the backend `commands::actions::ActionRequest`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionRequest {
    pub kind: ActionKind,
    pub device_ids: Vec<i64>,
    pub targets: Vec<String>,
    pub script_id: Option<i64>,
    pub script_uid: Option<String>,
    pub script_name: Option<String>,
    pub parameters: Option<String>,
    pub run_as: Option<String>,
    pub reboot: RebootChoice,
    pub reboot_mode: Option<RebootMode>,
    pub reason: Option<String>,
    pub include_offline: bool,
    pub override_window: bool,
    pub dry_run: bool,
    pub confirm_token: Option<String>,
}

impl ActionRequest {
    pub fn new(kind: ActionKind, device_ids: Vec<i64>) -> Self {
        Self {
            kind,
            device_ids,
            targets: Vec::new(),
            script_id: None,
            script_uid: None,
            script_name: None,
            parameters: None,
            run_as: None,
            reboot: RebootChoice::Never,
            reboot_mode: None,
            reason: None,
            include_offline: false,
            override_window: false,
            dry_run: false,
            confirm_token: None,
        }
    }
}

/// Payload of the `action:progress` event.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionProgressEvent {
    /// `dispatching` | `dispatched` | `polling` | `settled`
    pub stage: String,
    pub dispatched: usize,
    pub total: usize,
    #[serde(default)]
    pub jobs: Vec<JobReport>,
}
