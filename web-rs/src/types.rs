//! DTOs mirroring the Tauri backend. Serialized field names use camelCase to match
//! the backend's serde contract across the IPC boundary.
//!
//! These types are a hand-maintained mirror of the backend arg/result structs
//! (`src-tauri/src/{rows,model,commands}.rs`). A backend test,
//! `serialized_shapes_carry_every_frontend_required_key` in `src-tauri/src/rows.rs`,
//! fails if the backend drops/renames a key the mirrors below read — so drift is
//! caught in CI rather than as a silently blank column at runtime.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterParams {
    /// Selected organizations; empty = every organization. Multi-select, and the
    /// backend accepts a bare id here too so presets saved when these were scalars
    /// still load — see `src-tauri/src/filter.rs::ids`.
    pub organization_ids: Vec<i64>,
    /// Selected locations; empty = every location.
    pub location_ids: Vec<i64>,
    /// Selected device roles; empty = every role.
    pub role_ids: Vec<i64>,
    pub node_classes: Vec<String>,
    pub os_name_contains: Option<String>,
    pub search: Option<String>,
    /// Patch severities to keep (e.g. `CRITICAL`); empty = all.
    #[serde(default)]
    pub severities: Vec<String>,
    /// Release-date filter: relative window (last N days) and/or absolute bounds
    /// (Unix seconds) for a custom range.
    #[serde(default)]
    pub detected_within_days: Option<i64>,
    #[serde(default)]
    pub detected_after: Option<i64>,
    #[serde(default)]
    pub detected_before: Option<i64>,
    /// Absolute install-history range (Unix seconds). When `installed_after` is set
    /// it replaces the relative install lookback backend-side; both `None` = the
    /// lookback applies. Mirrors `filter::FilterParams::installed_*`.
    #[serde(default)]
    pub installed_after: Option<i64>,
    #[serde(default)]
    pub installed_before: Option<i64>,
}

/// Mirror of the backend's `rows::PatchFamilies` — the honest scope of every
/// compliance/severity/age number in a result.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchFamilies {
    #[serde(default)]
    pub os: bool,
    #[serde(default)]
    pub software: bool,
}

impl PatchFamilies {
    pub fn label(self) -> &'static str {
        match (self.os, self.software) {
            (true, true) => "OS and third-party patches",
            (true, false) => "OS patches only",
            (false, true) => "third-party patches only",
            (false, false) => "no patch families",
        }
    }

    /// Whether the rollups describe the whole backlog. `Default` is `false/false`,
    /// which reports as incomplete — the right way to fail if the field is ever
    /// missing from the wire.
    pub fn is_complete(self) -> bool {
        self.os && self.software
    }
}

#[derive(Clone, Debug, Deserialize)]
pub struct Organization {
    pub id: i64,
    pub name: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Location {
    pub id: i64,
    pub name: String,
    /// Which organization the location belongs to. The lookup now returns locations
    /// across every selected organization at once, so the list has to say which is
    /// which.
    #[serde(default)]
    pub organization_id: Option<i64>,
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
    /// Whether the row's device is flagged for reboot.
    ///
    /// The backend has always sent this; the mirror omitted it, so a grouped view
    /// could not derive its header flag from the rows it was grouping. The browser
    /// demo compensated with a side table keyed by device *name* — which is both a
    /// demo-only crutch and fragile, since a name is not an identity. Mirrored now,
    /// so both halves read the same field off the same row.
    #[serde(default)]
    pub needs_reboot: bool,
    pub patch_type: String,
    pub kb: Option<String>,
    pub name: String,
    pub severity: String,
    pub status: String,
    pub first_seen_date: Option<String>,
    pub installed_date: Option<String>,
    /// `DeviceSoftwarePatch.productIdentifier`, shared by every version of one
    /// third-party product; `None` on OS rows. Read by the demo's product grouping.
    #[serde(default)]
    pub product_identifier: Option<String>,
}

/// A device's facts and per-device rollup: the Needs-Reboot view's rows (the
/// backend sends only the reboot-needing subset there) and the drill-down's
/// header. Mirrors `rows::DeviceSummary`; the rollup fields default so an older
/// backend's payload still loads.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    /// What the Needs Reboot tab's device selection and the drill-down are keyed by.
    /// Always sent by the backend; defaulted only so a hand-built sample without it
    /// still deserializes.
    #[serde(default)]
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub location: Option<String>,
    pub device_role: Option<String>,
    pub os_name: Option<String>,
    /// Counted in the action bar's summary: an action against an offline device is
    /// queued, not run.
    #[serde(default)]
    pub offline: bool,
    pub pending_count: usize,
    #[serde(default)]
    pub needs_reboot: bool,
    #[serde(default)]
    pub rollup_scope: RollupScope,
    #[serde(default)]
    pub pending_by_severity: SeverityCounts,
    /// Pending Critical/Important past the SLA window (or undated).
    #[serde(default)]
    pub aged_critical: usize,
    /// `None` when the query did not include the Failed status — unknown, not zero.
    #[serde(default)]
    pub failed_installs: Option<usize>,
    #[serde(default)]
    pub last_contact: Option<String>,
}

/// Whether a device is in the population the fleet-health rollups describe, and
/// if not, why. Mirrors `rows::RollupScope`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RollupScope {
    #[default]
    Included,
    Offline,
    NonPatchable,
}

/// One device's drill-down, served from the backend's cached result by
/// `device_detail`. Mirrors `rows::DeviceDetail`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceDetail {
    /// `None` only for rows with no device in the scoped inventory.
    pub device: Option<DeviceSummary>,
    /// The device's rows as the Patches tab filters them, capped by the backend.
    pub rows: Vec<PatchRow>,
    pub rows_total: usize,
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
    /// Pending approval (vendor status `MANUAL`), any severity.
    #[serde(default)]
    pub awaiting_approval: usize,
    /// `APPROVED` and still not installed, any severity.
    #[serde(default)]
    pub approved_not_installed: usize,
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
    #[serde(default)]
    pub awaiting_approval: usize,
    #[serde(default)]
    pub approved_not_installed: usize,
}

/// Mirror of the backend `rows::ApprovalBacklog`: fleet totals of the two approval
/// columns and the devices whose approved patches are not installing. The
/// `stuck_devices` list is capped on the wire; `stuck_devices_total` is not.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ApprovalBacklog {
    pub awaiting_approval: usize,
    pub approved_not_installed: usize,
    pub stuck_after_days: i64,
    pub stuck_patches: usize,
    pub stuck_devices_total: usize,
    pub stuck_devices: Vec<StuckDevice>,
}

/// Mirror of the backend `rows::StuckDevice` (its `deviceId` and
/// `oldestFirstSeenTs` are not rendered, so not mirrored).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StuckDevice {
    pub device_name: String,
    pub organization: String,
    pub patches: usize,
    pub oldest_first_seen: Option<String>,
}

// Backend also sends severityRank and latestFailureTs; serde ignores undeclared
// fields. Only what the failures table renders is mirrored here.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureGroup {
    /// `OS` or `SOFTWARE`. Third-party failures carry no KB, so this is what tells
    /// the two apart in the table.
    #[serde(default)]
    pub patch_type: String,
    pub kb: Option<String>,
    pub name: String,
    pub severity: String,
    pub affected_devices: usize,
    /// Every affected device name (the full list, not a sample).
    pub device_names: Vec<String>,
    pub latest_failure: Option<String>,
}

/// Mirror of the backend's `changes::RunChanges`: the diff against the previous run
/// with the same tenant and facets. Counts are exact; the lists are capped.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RunChanges {
    /// When the run compared against ran. `None`: no previous comparable run.
    pub previous_at: Option<String>,
    pub tracks_pending: bool,
    pub tracks_failed: bool,
    pub too_large: bool,
    pub new_pending: usize,
    pub resolved: usize,
    pub newly_failed: usize,
    pub new_pending_items: Vec<ChangeItem>,
    pub resolved_items: Vec<ChangeItem>,
    pub newly_failed_items: Vec<ChangeItem>,
}

/// One patch on one device in a change list (backend `changes::ChangeItem`).
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ChangeItem {
    pub device_id: i64,
    pub device_name: String,
    pub patch_type: String,
    pub kb: Option<String>,
    pub name: String,
    pub severity: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityCounts {
    pub critical: usize,
    pub important: usize,
    pub security: usize,
    pub moderate: usize,
    pub recommended: usize,
    pub low: usize,
    pub optional: usize,
    pub unknown: usize,
}

/// One device's pending backlog. Mirrors `rows::DeviceBacklog`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBacklog {
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub os_name: Option<String>,
    pub pending: SeverityCounts,
    pub pending_total: usize,
    /// Pending records of any severity past their own band's SLA (undated counts).
    pub past_sla: usize,
    pub oldest_first_seen: Option<String>,
    /// The newest `timestamp` on the device's pending records — a collection time,
    /// not a last-contact time.
    pub latest_collected: Option<String>,
}

/// A capped device list plus how many devices qualified. Mirrors
/// `rows::DeviceBacklogList`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBacklogList {
    #[serde(default)]
    pub devices: Vec<DeviceBacklog>,
    #[serde(default)]
    pub devices_total: usize,
}

/// One first-seen → installed distribution, in days. Mirrors `rows::InstallLatency`.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallLatency {
    pub label: String,
    pub samples: usize,
    pub median_days: f64,
    pub p90_days: f64,
}

/// Mirrors `rows::TimeToInstall`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TimeToInstall {
    pub installs_queried: bool,
    pub overall: Option<InstallLatency>,
    pub by_organization: Vec<InstallLatency>,
    pub by_severity: Vec<InstallLatency>,
    pub installed_records: usize,
    pub excluded_records: usize,
}

/// Per-severity SLA overrides in days; `None` = use the default. Mirrors
/// `settings::SlaBySeverity` and round-trips through the Settings panel.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlaBySeverity {
    pub critical: Option<i64>,
    pub important: Option<i64>,
    pub security: Option<i64>,
    pub moderate: Option<i64>,
    pub recommended: Option<i64>,
    pub low: Option<i64>,
    pub optional: Option<i64>,
}

/// One overridable band: its label, and how to read and write its override.
pub type SlaBand = (
    &'static str,
    fn(&SlaBySeverity) -> Option<i64>,
    fn(&mut SlaBySeverity) -> &mut Option<i64>,
);

impl SlaBySeverity {
    /// Every overridable band, most urgent first. `Unknown` has no override — it
    /// always takes the default, as in the backend.
    pub const BANDS: [SlaBand; 7] = [
        ("Critical", |s| s.critical, |s| &mut s.critical),
        ("Important", |s| s.important, |s| &mut s.important),
        ("Security", |s| s.security, |s| &mut s.security),
        ("Moderate", |s| s.moderate, |s| &mut s.moderate),
        ("Recommended", |s| s.recommended, |s| &mut s.recommended),
        ("Low", |s| s.low, |s| &mut s.low),
        ("Optional", |s| s.optional, |s| &mut s.optional),
    ];
}

/// The SLA policy a result was computed under. Mirrors `settings::SlaPolicy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SlaPolicy {
    pub default_days: i64,
    pub by_severity: SlaBySeverity,
}

impl Default for SlaPolicy {
    fn default() -> Self {
        Self {
            default_days: 30,
            by_severity: SlaBySeverity::default(),
        }
    }
}

/// Which key the Patches view groups its rows by. Mirrors `rows::GroupBy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GroupBy {
    Device,
    Patch,
    /// Third-party rows by product identifier (every version in one group); every
    /// other row by its patch key.
    Product,
}

/// One collapsed group header. Mirrors `rows::PatchGroup`. Members are fetched
/// separately via `get_patch_group_members` — a patch group can span the whole
/// fleet, so its rows stay off the wire until the operator expands it.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchGroup {
    pub key: String,
    pub label: String,
    pub sublabel: Option<String>,
    pub rows: usize,
    pub devices: usize,
    pub severity: String,
    pub severity_rank: u8,
    pub offline: bool,
    pub needs_reboot: bool,
}

/// One page of group headers plus the total. Mirrors `rows::GroupPage`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupPage {
    #[serde(default)]
    pub groups: Vec<PatchGroup>,
    #[serde(default)]
    pub total: usize,
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
    /// The worst online devices by pending backlog (capped).
    #[serde(default)]
    pub worst_devices: DeviceBacklogList,
    /// Offline devices still listed with pending patches (capped).
    #[serde(default)]
    pub offline_backlog: DeviceBacklogList,
    /// First seen → installed, by organization and severity.
    #[serde(default)]
    pub time_to_install: TimeToInstall,
    /// The SLA policy this result's aging figures were computed under — may differ
    /// from Settings if the policy changed since.
    #[serde(default)]
    pub sla_policy: SlaPolicy,
    /// Approval workflow totals and the stuck-approval devices.
    #[serde(default)]
    pub approvals: ApprovalBacklog,
    pub devices_total: usize,
    /// How many of `devices_total` are offline. The compliance rollups exclude them
    /// from both the denominator and the pending counts, so the Devices column of the
    /// compliance table does not sum to `devices_total` — this is what lets the UI
    /// say so instead of showing two device counts with no explanation.
    #[serde(default)]
    pub devices_offline: usize,
    /// How many of `devices_total` are online but not something NinjaOne patch
    /// management covers (switches, printers, hypervisors, cloud monitors). Excluded
    /// from the rollups like the offline devices, and named in the scope note.
    #[serde(default)]
    pub devices_unpatchable: usize,
    /// Which patch families the fleet-health rollups actually cover.
    #[serde(default)]
    pub patch_families: PatchFamilies,
    /// What changed since the previous comparable run (backend `changes::RunChanges`).
    #[serde(default)]
    pub changes: RunChanges,
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
    #[serde(default)]
    pub sla_by_severity: SlaBySeverity,
    pub has_client_secret: bool,
    pub presets: Vec<Preset>,
    pub auto_check_updates: bool,
    #[serde(default)]
    pub actions: ActionSettings,
    /// Set by `save_settings` when the save switched instance or client id. The
    /// backend has already dropped its cached result at that point, so anything
    /// still on screen belongs to a tenant this session can no longer page or
    /// export. `get_settings` always sends `false`.
    #[serde(default)]
    pub tenant_changed: bool,
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

// `Deserialize` only so a shared view link can name a sort column by the same
// spelling the backend uses; see `util::view_link`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
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
    FirstSeenDate,
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
    pub sla_by_severity: SlaBySeverity,
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
    OsPatchRemediate,
    SoftwarePatchRemediate,
    Reboot,
    Script,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::OsPatchScan => "Scan for OS patches",
            Self::SoftwarePatchScan => "Scan for software patches",
            Self::OsPatchApply => "Apply all OS patches",
            Self::SoftwarePatchApply => "Apply all software patches",
            Self::OsPatchRemediate => "Apply selected OS patches",
            Self::SoftwarePatchRemediate => "Apply selected software patches",
            Self::Reboot => "Reboot",
            Self::Script => "Run script",
        }
    }

    /// What this action actually reaches, in the operator's terms.
    ///
    /// The blast radius used to live only in an 11px muted `ACTION_GROUPS` heading
    /// above a pair of buttons both labelled "OS" and "Software", and in ~35 lines
    /// of README prose calling the distinction "the single most important thing to
    /// know here". Two buttons that differ by *installs the three KBs you ticked*
    /// versus *installs this device's entire approved backlog* were separated by the
    /// smallest, dimmest text on screen. Documentation was compensating for an
    /// affordance that was not there.
    ///
    /// Used three ways so the reach is unmissable however the operator reads: the
    /// button's accessible name and tooltip, and the confirmation dialog's first
    /// line.
    pub fn blast_radius(self) -> &'static str {
        match self {
            Self::OsPatchScan | Self::SoftwarePatchScan => {
                "Scans the device. Installs nothing and changes nothing."
            }
            Self::OsPatchApply => {
                "Installs EVERY approved OS patch on each selected device — not just the rows you ticked."
            }
            Self::SoftwarePatchApply => {
                "Installs EVERY approved software patch on each selected device — not just the rows you ticked."
            }
            Self::OsPatchRemediate => {
                "Installs only the OS patches you ticked, and each device receives only its own."
            }
            Self::SoftwarePatchRemediate => {
                "Installs only the software patches you ticked, and each device receives only its own."
            }
            Self::Reboot => "Restarts each selected device.",
            Self::Script => "Runs the chosen library script on each selected device.",
        }
    }

    /// Whether this action's reach is wider than the operator's selection.
    ///
    /// True only for the two native apply endpoints, which take no target list —
    /// NinjaOne has no per-patch apply, so `/patch/{os,software}/apply` installs the
    /// device's whole approved backlog. Drives the warning styling that makes those
    /// two buttons read differently from every other one.
    pub fn exceeds_selection(self) -> bool {
        matches!(self, Self::OsPatchApply | Self::SoftwarePatchApply)
    }

    /// Mirrors the backend rule: scans don't change the device, so they need no
    /// confirmation. Display-only here — the backend enforces it for real.
    pub fn is_mutating(self) -> bool {
        !matches!(self, Self::OsPatchScan | Self::SoftwarePatchScan)
    }

    /// Every variant, so a test can enumerate them rather than trusting a
    /// hand-written list to stay complete. These predicates decide whether the
    /// operator sees a confirmation dialog and how the blast radius is described,
    /// and they are a hand-mirrored copy of the backend's `actions::ActionKind` —
    /// the crates share no code, so nothing but a test can notice drift.
    ///
    /// Test-only: nothing in the app iterates the kinds (the ActionBar's
    /// `ACTION_GROUPS` names them individually so each sits under the heading that
    /// describes its blast radius), and the wasm build excludes `#[cfg(test)]`.
    #[cfg(test)]
    pub const ALL: [Self; 8] = [
        Self::OsPatchScan,
        Self::SoftwarePatchScan,
        Self::OsPatchApply,
        Self::SoftwarePatchApply,
        Self::OsPatchRemediate,
        Self::SoftwarePatchRemediate,
        Self::Reboot,
        Self::Script,
    ];

    /// Whether this action can restart the device as a side effect.
    pub fn can_reboot(self) -> bool {
        !matches!(self, Self::OsPatchScan | Self::SoftwarePatchScan)
    }

    /// Whether this installs *only* the ticked patches, via the remediation script
    /// configured for its family. Mirrors the backend; see `actions::ActionKind`.
    pub fn is_remediation(self) -> bool {
        matches!(self, Self::OsPatchRemediate | Self::SoftwarePatchRemediate)
    }

    /// Whether this dispatches a library script rather than a native endpoint.
    pub fn runs_a_script(self) -> bool {
        matches!(
            self,
            Self::Script | Self::OsPatchRemediate | Self::SoftwarePatchRemediate
        )
    }

    /// Whether this targets the OS patch family (vs third-party software).
    pub fn is_os_family(self) -> bool {
        matches!(
            self,
            Self::OsPatchScan | Self::OsPatchApply | Self::OsPatchRemediate
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RebootMode {
    Normal,
    Forced,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
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
    /// Mirrors the backend's `JobState::is_terminal`. `Unknown` is deliberately not
    /// terminal: a timed-out dispatch is polled until `/activities` resolves it.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed(_) | Self::TimedOut | Self::Skipped(_)
        )
    }

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

// The backend also sends dispatchedTs and finishedAt; serde ignores fields not
// declared here. The correlators (`activityId`/`seriesUid`) ARE kept: NinjaOne v2
// has no script-output endpoint, so they are how an operator finds the run in the
// NinjaOne console.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobReport {
    pub id: u64,
    /// Groups a batch's rows for "Retry failed".
    pub batch_id: u64,
    /// The device a retry is dispatched to.
    pub device_id: i64,
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
    /// What a retry is rebuilt from. `None` when the backend recorded no request.
    #[serde(default)]
    pub request: Option<JobRequest>,
}

/// Mirror of the backend `actions::JobRequest`: the inputs of the dispatch that
/// produced one job, for that one device. `kind` and `dry_run` live on the
/// [`JobReport`]; the maintenance-window override is deliberately not recorded.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobRequest {
    pub script_id: Option<i64>,
    pub script_uid: Option<String>,
    pub script_name: Option<String>,
    pub parameters: Option<String>,
    pub run_as: Option<String>,
    #[serde(default)]
    pub reboot: RebootChoice,
    pub reboot_mode: Option<RebootMode>,
    pub reason: Option<String>,
    #[serde(default)]
    pub include_offline: bool,
    #[serde(default)]
    pub targets: Vec<String>,
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
    /// What a native "Apply all" will install, per device. Absent for every other
    /// kind (and from a backend that predates it).
    #[serde(default)]
    pub apply_preview: Option<ApplyPreview>,
    /// This dispatch goes out only because it overrides a closed maintenance window.
    #[serde(default)]
    pub window_overridden: bool,
    /// Absent when the plan is blocked — there is nothing to confirm.
    pub confirm_token: Option<String>,
}

/// Mirror of the backend `actions::ApplyPreview`: what NinjaOne's apply endpoint
/// will install, counted from the cached whole-fleet current patches.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPreview {
    /// `"OS"` or `"software"`.
    pub family: String,
    /// False when the patch data was not loaded: the counts are unknown, not zero.
    pub known: bool,
    pub devices: Vec<ApplyPreviewDevice>,
    pub approved_total: usize,
    pub pending_manual_total: usize,
    pub data_fetched_at: Option<String>,
}

/// Mirror of the backend `actions::ApplyPreviewDevice`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPreviewDevice {
    pub device_id: i64,
    pub device_name: String,
    /// `APPROVED` records — what the apply installs.
    pub approved: usize,
    /// `MANUAL` records — pending approval in NinjaOne, which the apply skips.
    pub pending_manual: usize,
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
    /// Whether the library entry declares a `dryRun` variable. A dry run only
    /// appends `dryRun=true`, so a script that doesn't read it would run for real;
    /// the backend refuses such a dry run and the action bar says so up front.
    #[serde(default)]
    pub accepts_dry_run: bool,
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
    /// Device id → the ticked KBs (OS) or product titles (software) that device
    /// should install. Serialized with string keys because JSON object keys are
    /// strings; the backend's `HashMap<i64, _>` deserializes them.
    pub device_targets: BTreeMap<i64, Vec<String>>,
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
            device_targets: BTreeMap::new(),
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

/// One dispatched action from the durable audit trail. Mirrors
/// `commands::diagnostics::AuditRecord`; every field is tolerant because the log is
/// append-only across app versions and older records predate newer fields.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AuditRecord {
    pub timestamp: String,
    pub kind: String,
    pub device_name: String,
    pub organization: String,
    pub detail: String,
    pub outcome: String,
    pub dry_run: bool,
    /// The dispatch overrode a closed maintenance window.
    pub window_override: bool,
    pub batch_id: Option<u64>,
    pub exit_code: Option<i32>,
    /// Written by a build that used the pre-`paths::app_dir` directory.
    pub legacy: bool,
}

/// One completed query's fleet-health numbers, from the run-history trail. Mirrors
/// `history::RunRecord`. Every field is a scalar whose meaning is stable across app
/// versions — derived values (percentages) are recomputed here, never frozen on disk.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct RunRecord {
    pub at: String,
    pub instance: String,
    pub devices_total: usize,
    pub devices_offline: usize,
    pub devices_unpatchable: usize,
    pub devices_compliant: usize,
    pub devices_in_scope: usize,
    pub rows_total: usize,
    pub pending_critical: usize,
    pub aged_critical: usize,
    pub failures: usize,
    pub needs_reboot: usize,
    pub os_patches: bool,
    pub software_patches: bool,
    pub scoped: bool,
    /// Canonical spelling of every facet the run applied. Two runs with different
    /// keys (org A vs org B, or a severity-only run) are not one series. Empty on
    /// lines written before the key existed.
    pub scope_key: String,
    /// Per-organization numbers, largest organizations first and capped backend-side
    /// (`orgs_total` says how many there were). Empty on old lines, and on lines
    /// the backend returns without detail (all but its newest few hundred).
    pub orgs: Vec<OrgRun>,
    pub orgs_total: usize,
}

/// One organization in one run (backend `history::OrgRun`). Short keys on the wire
/// because the entry repeats per org on every history line.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default)]
pub struct OrgRun {
    #[serde(rename = "o")]
    pub organization: String,
    #[serde(rename = "n")]
    pub devices_in_scope: usize,
    #[serde(rename = "c")]
    pub devices_compliant: usize,
    #[serde(rename = "p")]
    pub pending: usize,
    #[serde(rename = "pc")]
    pub pending_critical: usize,
    #[serde(rename = "ac")]
    pub aged_critical: usize,
}

impl OrgRun {
    /// Same rule as [`RunRecord::compliance_pct`]: an empty org has no percentage.
    pub fn compliance_pct(&self) -> Option<f64> {
        (self.devices_in_scope > 0)
            .then(|| self.devices_compliant as f64 * 100.0 / self.devices_in_scope as f64)
    }
}

impl RunRecord {
    /// Compliance over the population the rollups cover. `None` for an empty scope
    /// rather than 0% — an empty scope is not a fleet at zero compliance, and
    /// charting it as one is the same class of lie as rounding 99.5% up to 100.
    /// The backend stores only the two counts; the percentage is derived here.
    pub fn compliance_pct(&self) -> Option<f64> {
        (self.devices_in_scope > 0)
            .then(|| self.devices_compliant as f64 * 100.0 / self.devices_in_scope as f64)
    }

    /// Which families these numbers cover, in the same words `PatchFamilies::label`
    /// uses so the trend header and the compliance scope note agree.
    pub fn patch_families_label(&self) -> &'static str {
        PatchFamilies {
            os: self.os_patches,
            software: self.software_patches,
        }
        .label()
    }

    /// Whether two records measured the same thing, and so belong on one trend line.
    pub fn comparable_with(&self, other: &Self) -> bool {
        self.instance == other.instance
            && self.os_patches == other.os_patches
            && self.software_patches == other.software_patches
            && self.scoped == other.scoped
            && self.scope_key == other.scope_key
    }
}
