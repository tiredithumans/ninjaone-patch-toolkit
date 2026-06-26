//! Joins device inventory with patch records to produce the flat per-server patch
//! rows the UI lists and the Excel exporter writes, plus the device rollups that
//! drive the reboot and compliance views.
//!
//! Adapted from `ninjaone-patch-dashboard`'s `snapshot.rs` device↔patch join.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::filter::FilterParams;
use crate::model::{Device, Location, Organization, Patch, PatchRow, Role, Severity};

/// Id→name maps used to label patch rows without repeated lookups.
pub struct LookupMaps {
    pub orgs: HashMap<i64, String>,
    pub locations: HashMap<i64, String>,
    pub roles: HashMap<i64, String>,
}

impl LookupMaps {
    pub fn build(orgs: &[Organization], locations: &[Location], roles: &[Role]) -> Self {
        Self {
            orgs: orgs.iter().map(|o| (o.id, o.name.clone())).collect(),
            locations: locations.iter().map(|l| (l.id, l.name.clone())).collect(),
            roles: roles.iter().map(|r| (r.id, r.name.clone())).collect(),
        }
    }

    fn org_name(&self, id: Option<i64>) -> String {
        id.and_then(|i| self.orgs.get(&i))
            .cloned()
            .unwrap_or_else(|| "(unknown)".to_string())
    }

    fn location_name(&self, id: Option<i64>) -> Option<String> {
        id.and_then(|i| self.locations.get(&i)).cloned()
    }

    fn role_name(&self, id: Option<i64>) -> Option<String> {
        id.and_then(|i| self.roles.get(&i)).cloned()
    }
}

/// One slice of fetched patches tagged with its family and (for installs) a status
/// to apply when the record omits one.
pub struct PatchSource<'a> {
    pub patches: &'a [Patch],
    pub type_label: &'static str,
    pub status_override: Option<&'static str>,
    /// When set, only patches whose raw status (or, if absent, `status_override`)
    /// is in this set become rows — lets the caller narrow a patch family to the
    /// requested statuses without cloning the matched subset out first. Used for
    /// both the current-patch families (MANUAL/APPROVED/REJECTED) and the install
    /// families, which return both INSTALLED and FAILED records and so are narrowed
    /// to the requested install statuses.
    pub status_filter: Option<&'a HashSet<&'static str>>,
}

fn fmt_dt(ts: Option<DateTime<Utc>>) -> Option<String> {
    ts.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
}

/// Maps a raw NinjaOne patch status to the operator-facing label. NinjaOne uses
/// `MANUAL` for patches pending approval; show that as `PENDING` so the table
/// matches the Status filter (and NinjaOne's own UI, which labels them "Pending").
fn display_status(raw: &str) -> String {
    match raw {
        "MANUAL" => "PENDING".to_string(),
        other => other.to_string(),
    }
}

/// Builds detail rows from the given patch sources, resolving device/org/location/
/// role/OS names and applying the client-side OS-name and free-text filters.
pub fn build_rows(
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
    sources: &[PatchSource<'_>],
    filter: &FilterParams,
) -> Vec<PatchRow> {
    let mut rows = Vec::new();
    // Lower the query needles and parse the severities once, not per patch.
    let prepared = filter.prepare();
    for source in sources {
        for patch in source.patches {
            if let Some(allowed) = source.status_filter {
                // Fall back to the source's status_override when a record omits its
                // own status, so an install record with no status still matches the
                // label (e.g. INSTALLED) it would be displayed under.
                let keep = patch
                    .status
                    .as_deref()
                    .or(source.status_override)
                    .map(|s| allowed.contains(s))
                    .unwrap_or(false);
                if !keep {
                    continue;
                }
            }
            let device = patch
                .device_id
                .and_then(|id| devices_by_id.get(&id))
                .copied();
            // NinjaOne's /queries/* patch endpoints ignore `class` in `df`, so the
            // node-class facet is applied here: `devices_by_id` is already
            // class-filtered (the device query does honor `class`), so when a class
            // is selected, drop patches whose device isn't in that set.
            if !filter.node_classes.is_empty() && device.is_none() {
                continue;
            }
            let os_name = device.and_then(Device::os_name);

            if !prepared.os_name_allowed(os_name.as_deref()) {
                continue;
            }
            if !prepared.search_allowed(patch.kb_number.as_deref(), patch.name.as_deref()) {
                continue;
            }

            let severity = patch.severity_enum();
            if !prepared.severity_allowed(severity) {
                continue;
            }
            let released = patch.released_at();
            if !prepared.release_date_allowed(released.map(|r| r.timestamp())) {
                continue;
            }
            let status = patch
                .status
                .as_deref()
                .or(source.status_override)
                .map(display_status)
                .unwrap_or_else(|| "UNKNOWN".to_string());

            rows.push(PatchRow {
                device_id: patch.device_id.unwrap_or_default(),
                device_name: device
                    .map(|d| d.label().to_string())
                    .unwrap_or_else(|| "(unknown)".to_string()),
                organization: maps.org_name(device.and_then(|d| d.organization_id)),
                location: maps.location_name(device.and_then(|d| d.location_id)),
                device_role: maps.role_name(device.and_then(|d| d.node_role_id)),
                os_name,
                node_class: device.and_then(|d| d.node_class.clone()),
                needs_reboot: device.map(|d| d.needs_reboot()).unwrap_or(false),
                patch_type: source.type_label.to_string(),
                kb: patch.kb_number.clone(),
                name: patch.display_name(),
                severity: severity.label().to_string(),
                severity_rank: severity.rank(),
                status,
                release_date: fmt_dt(released),
                installed_date: fmt_dt(patch.installed_at()),
                release_ts: patch.release_timestamp.map(|s| s as i64),
                installed_ts: patch.installed_timestamp.map(|s| s as i64),
            });
        }
    }
    rows
}

/// A device-level rollup for the reboot view and compliance computation.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSummary {
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub location: Option<String>,
    pub device_role: Option<String>,
    pub os_name: Option<String>,
    pub node_class: Option<String>,
    pub needs_reboot: bool,
    pub pending_count: usize,
}

pub fn build_device_summaries(
    devices: &[&Device],
    pending_counts: &HashMap<i64, usize>,
    maps: &LookupMaps,
) -> Vec<DeviceSummary> {
    devices
        .iter()
        .map(|d| DeviceSummary {
            device_id: d.id,
            device_name: d.label().to_string(),
            organization: maps.org_name(d.organization_id),
            location: maps.location_name(d.location_id),
            device_role: maps.role_name(d.node_role_id),
            os_name: d.os_name(),
            node_class: d.node_class.clone(),
            needs_reboot: d.needs_reboot(),
            pending_count: pending_counts.get(&d.id).copied().unwrap_or(0),
        })
        .collect()
}

/// Per-organization compliance rollup for the summary view and Excel summary sheet.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceBucket {
    pub organization: String,
    pub devices_total: usize,
    pub devices_compliant: usize,
    pub compliance_pct: f64,
    pub pending_critical: usize,
    /// Pending Critical/Important patches whose release date is older than the SLA
    /// window — the backlog that has aged past target.
    pub aged_critical: usize,
}

/// Computes per-org compliance from device summaries and the current (pending/
/// approved) patches. `sla_days` flags aged Critical/Important backlog.
pub fn build_compliance(
    summaries: &[DeviceSummary],
    current_patches: &[Patch],
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
    sla_days: i64,
    now: DateTime<Utc>,
) -> Vec<ComplianceBucket> {
    #[derive(Default)]
    struct Acc {
        total: usize,
        compliant: usize,
        pending_critical: usize,
        aged_critical: usize,
    }
    let mut by_org: HashMap<String, Acc> = HashMap::new();

    for s in summaries {
        // An offline device can't apply patches and reports no current patch
        // records, so a zero pending count says nothing about its compliance.
        // Exclude it from the denominator rather than scoring it compliant and
        // inflating the headline metric.
        let offline = devices_by_id
            .get(&s.device_id)
            .map(|d| d.is_offline())
            .unwrap_or(false);
        if offline {
            continue;
        }
        let acc = by_org.entry(s.organization.clone()).or_default();
        acc.total += 1;
        if s.pending_count == 0 {
            acc.compliant += 1;
        }
    }

    let sla_cutoff = now - Duration::days(sla_days);
    for p in current_patches {
        // NinjaOne uses MANUAL (pending approval) and APPROVED for current patches
        // not yet installed — both count toward the pending backlog.
        let is_pending = matches!(p.status.as_deref(), Some("MANUAL") | Some("APPROVED"));
        if !is_pending {
            continue;
        }
        let sev = p.severity_enum();
        if sev.rank() < Severity::Important.rank() {
            continue;
        }
        let org = p
            .device_id
            .and_then(|id| devices_by_id.get(&id))
            .map(|d| maps.org_name(d.organization_id))
            .unwrap_or_else(|| "(unknown)".to_string());
        let acc = by_org.entry(org).or_default();
        acc.pending_critical += 1;
        // A pending Critical/Important patch with no known release date can't be
        // proven fresh, so flag it as aged for review rather than assuming it is
        // within SLA (which would understate the backlog).
        if p.released_at().map(|r| r < sla_cutoff).unwrap_or(true) {
            acc.aged_critical += 1;
        }
    }

    let mut buckets: Vec<ComplianceBucket> = by_org
        .into_iter()
        .map(|(organization, a)| ComplianceBucket {
            organization,
            devices_total: a.total,
            devices_compliant: a.compliant,
            compliance_pct: if a.total == 0 {
                100.0
            } else {
                (a.compliant as f64 / a.total as f64) * 100.0
            },
            pending_critical: a.pending_critical,
            aged_critical: a.aged_critical,
        })
        .collect();
    buckets.sort_by_cached_key(|b| b.organization.to_lowercase());
    buckets
}

/// Counts current pending/approved patches per device for compliance and the
/// reboot/summary views. NinjaOne uses `MANUAL` for pending-approval patches.
pub fn pending_counts(current_patches: &[Patch]) -> HashMap<i64, usize> {
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for p in current_patches {
        if matches!(p.status.as_deref(), Some("MANUAL") | Some("APPROVED"))
            && let Some(id) = p.device_id
        {
            *counts.entry(id).or_default() += 1;
        }
    }
    counts
}

/// A fleet-wide rollup of FAILED install records grouped by patch, so the operator
/// can see which patches are failing across the most devices during a patch cycle.
/// Built from the FAILED rows already present in the result — no extra fetch.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureGroup {
    pub patch_type: String,
    pub kb: Option<String>,
    pub name: String,
    pub severity: String,
    pub severity_rank: u8,
    /// Distinct devices the patch failed on (the headline count).
    pub affected_devices: usize,
    /// Every affected device name, so the table and Excel/HTML export carry the
    /// complete list (not a truncated sample).
    pub device_names: Vec<String>,
    pub latest_failure: Option<String>,
    pub latest_failure_ts: Option<i64>,
}

/// Pending-patch counts by MSRC severity bucket, for the dashboard breakdown.
#[derive(Debug, Clone, Copy, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeverityCounts {
    pub critical: usize,
    pub important: usize,
    pub moderate: usize,
    pub low: usize,
    pub optional: usize,
    pub unknown: usize,
}

/// A per-organization pending-patch severity breakdown for the dashboard charts.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgSeverity {
    pub organization: String,
    pub counts: SeverityCounts,
}

/// One bucket of the pending-patch age histogram (by release age).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgeBucket {
    pub label: String,
    pub count: usize,
}

/// Whether a current-patch status counts toward the pending backlog. NinjaOne uses
/// `MANUAL` (pending approval) and `APPROVED` for patches not yet installed.
fn is_pending(status: Option<&str>) -> bool {
    matches!(status, Some("MANUAL") | Some("APPROVED"))
}

/// Groups the FAILED detail rows by patch (`patch_type` + `kb` + `name`), counting
/// the distinct devices each failed on, the most recent failure, and the full list
/// of affected device names. Sorted by affected-device count then severity, desc.
pub fn build_failures(rows: &[PatchRow]) -> Vec<FailureGroup> {
    struct Acc {
        patch_type: String,
        kb: Option<String>,
        name: String,
        severity: String,
        severity_rank: u8,
        devices: HashSet<i64>,
        device_names: Vec<String>,
        latest_ts: Option<i64>,
        latest_date: Option<String>,
    }
    let mut groups: HashMap<(String, Option<String>, String), Acc> = HashMap::new();
    for r in rows {
        if r.status != "FAILED" {
            continue;
        }
        let acc = groups
            .entry((r.patch_type.clone(), r.kb.clone(), r.name.clone()))
            .or_insert_with(|| Acc {
                patch_type: r.patch_type.clone(),
                kb: r.kb.clone(),
                name: r.name.clone(),
                severity: r.severity.clone(),
                severity_rank: r.severity_rank,
                devices: HashSet::new(),
                device_names: Vec::new(),
                latest_ts: None,
                latest_date: None,
            });
        // Count distinct devices by id, but only add a name the first time we see
        // that device, so the name list has no duplicates.
        if acc.devices.insert(r.device_id) {
            acc.device_names.push(r.device_name.clone());
        }
        // Surface the highest severity seen for the group (records can disagree).
        if r.severity_rank > acc.severity_rank {
            acc.severity_rank = r.severity_rank;
            acc.severity = r.severity.clone();
        }
        if let Some(ts) = r.installed_ts
            && acc.latest_ts.map(|cur| ts > cur).unwrap_or(true)
        {
            acc.latest_ts = Some(ts);
            acc.latest_date = r.installed_date.clone();
        }
    }
    let mut out: Vec<FailureGroup> = groups
        .into_values()
        .map(|a| FailureGroup {
            patch_type: a.patch_type,
            kb: a.kb,
            name: a.name,
            severity: a.severity,
            severity_rank: a.severity_rank,
            affected_devices: a.devices.len(),
            device_names: a.device_names,
            latest_failure: a.latest_date,
            latest_failure_ts: a.latest_ts,
        })
        .collect();
    out.sort_by_cached_key(|g| (Reverse(g.affected_devices), Reverse(g.severity_rank)));
    out
}

/// Buckets pending (MANUAL/APPROVED) current patches by org and MSRC severity for
/// the dashboard's severity breakdown. Sorted by organization name.
pub fn build_severity_by_org(
    current_patches: &[Patch],
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
) -> Vec<OrgSeverity> {
    let mut by_org: HashMap<String, SeverityCounts> = HashMap::new();
    for p in current_patches {
        if !is_pending(p.status.as_deref()) {
            continue;
        }
        let org = p
            .device_id
            .and_then(|id| devices_by_id.get(&id))
            .map(|d| maps.org_name(d.organization_id))
            .unwrap_or_else(|| "(unknown)".to_string());
        let counts = by_org.entry(org).or_default();
        match p.severity_enum() {
            Severity::Critical => counts.critical += 1,
            Severity::Important => counts.important += 1,
            Severity::Moderate => counts.moderate += 1,
            Severity::Low => counts.low += 1,
            Severity::Optional => counts.optional += 1,
            Severity::Unknown => counts.unknown += 1,
        }
    }
    let mut out: Vec<OrgSeverity> = by_org
        .into_iter()
        .map(|(organization, counts)| OrgSeverity {
            organization,
            counts,
        })
        .collect();
    out.sort_by_cached_key(|o| o.organization.to_lowercase());
    out
}

/// Fixed labels for the pending-patch age histogram, oldest bucket last.
const AGE_BUCKET_LABELS: [&str; 5] = [
    "0-30 days",
    "31-60 days",
    "61-90 days",
    "91-180 days",
    "180+ days",
];

/// Builds the pending-patch age histogram from release age. A pending patch with no
/// known release date can't be proven fresh, so it falls into the oldest bucket —
/// the same "can't prove fresh → aged" convention `build_compliance` uses.
pub fn build_age_buckets(current_patches: &[Patch], now: DateTime<Utc>) -> Vec<AgeBucket> {
    let mut counts = [0usize; 5];
    for p in current_patches {
        if !is_pending(p.status.as_deref()) {
            continue;
        }
        let age_days = p
            .released_at()
            .map(|r| (now - r).num_days().max(0))
            .unwrap_or(i64::MAX);
        let idx = match age_days {
            0..=30 => 0,
            31..=60 => 1,
            61..=90 => 2,
            91..=180 => 3,
            _ => 4,
        };
        counts[idx] += 1;
    }
    AGE_BUCKET_LABELS
        .iter()
        .zip(counts)
        .map(|(label, count)| AgeBucket {
            label: (*label).to_string(),
            count,
        })
        .collect()
}

/// The full result of a patch query. Cached in `AppState.last_result` and read by
/// the Excel exporter; **not** sent wholesale over IPC — the frontend gets a
/// [`QuerySummary`] and pages the detail rows on demand via `get_patch_rows`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryResult {
    pub rows: Vec<PatchRow>,
    pub devices: Vec<DeviceSummary>,
    pub compliance: Vec<ComplianceBucket>,
    /// FAILED-install rollup (empty unless the FAILED status was queried).
    pub failures: Vec<FailureGroup>,
    /// Per-org pending-patch severity breakdown for the dashboard.
    pub severity_by_org: Vec<OrgSeverity>,
    /// Pending-patch age histogram for the dashboard.
    pub age_buckets: Vec<AgeBucket>,
    pub devices_total: usize,
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
    /// FAILED-install rollup — small (one entry per failing patch), so it ships
    /// whole rather than paged like the detail rows.
    pub failures: Vec<FailureGroup>,
    /// Per-org pending-patch severity breakdown for the dashboard charts.
    pub severity_by_org: Vec<OrgSeverity>,
    /// Pending-patch age histogram for the dashboard charts.
    pub age_buckets: Vec<AgeBucket>,
    pub devices_total: usize,
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
            failures: result.failures.clone(),
            severity_by_org: result.severity_by_org.clone(),
            age_buckets: result.age_buckets.clone(),
            devices_total: result.devices_total,
            generated_at: result.generated_at.clone(),
            data_fetched_at: result.data_fetched_at.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::OsInfo;

    fn device(id: i64, org: i64, os: &str) -> Device {
        Device {
            id,
            system_name: Some(format!("srv{id}")),
            display_name: Some(format!("srv{id}")),
            organization_id: Some(org),
            location_id: Some(100),
            node_role_id: Some(2),
            node_class: Some("WINDOWS_SERVER".into()),
            offline: Some(false),
            os: Some(OsInfo {
                name: Some(os.into()),
                needs_reboot: Some(id % 2 == 0),
            }),
        }
    }

    fn patch(device_id: i64, status: &str, sev: &str, released_days_ago: Option<i64>) -> Patch {
        Patch {
            device_id: Some(device_id),
            kb_number: Some("KB5040434".into()),
            name: Some("Cumulative Update".into()),
            version: None,
            product_vendor: None,
            severity: Some(sev.into()),
            status: Some(status.into()),
            patch_type: None,
            release_timestamp: released_days_ago
                .map(|d| (Utc::now() - Duration::days(d)).timestamp() as f64),
            installed_timestamp: None,
        }
    }

    fn maps() -> LookupMaps {
        LookupMaps {
            orgs: HashMap::from([(10, "Contoso".to_string())]),
            locations: HashMap::from([(100, "HQ".to_string())]),
            roles: HashMap::from([(2, "Domain Controller".to_string())]),
        }
    }

    #[test]
    fn build_rows_resolves_names_and_applies_os_filter() {
        let d1 = device(1, 10, "Windows Server 2022");
        let d2 = device(2, 10, "Windows Server 2019");
        let by_id = HashMap::from([(1, &d1), (2, &d2)]);
        let patches = vec![
            patch(1, "PENDING", "CRITICAL", Some(5)),
            patch(2, "PENDING", "LOW", Some(5)),
        ];
        let maps = maps();
        let filter = FilterParams {
            os_name_contains: Some("2022".into()),
            ..Default::default()
        };
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &filter,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].organization, "Contoso");
        assert_eq!(rows[0].location.as_deref(), Some("HQ"));
        assert_eq!(rows[0].device_role.as_deref(), Some("Domain Controller"));
        assert_eq!(rows[0].patch_type, "OS");
    }

    #[test]
    fn release_date_filter_narrows_rows() {
        let d1 = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d1)]);
        let maps = maps();
        let patches = vec![
            patch(1, "PENDING", "CRITICAL", Some(2)), // released 2 days ago → kept
            patch(1, "PENDING", "CRITICAL", Some(100)), // released 100 days ago → dropped
        ];
        let cutoff = (Utc::now() - Duration::days(10)).timestamp();
        let filter = FilterParams {
            release_after: Some(cutoff),
            ..Default::default()
        };
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &filter,
        );
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn node_class_filter_drops_patches_without_a_matched_device() {
        // The patch query isn't class-filtered server-side, so build_rows narrows
        // it to patches whose device is in the (class-filtered) device set.
        let d1 = device(1, 10, "Linux"); // matched the class → in the device map
        let by_id = HashMap::from([(1, &d1)]);
        let patches = vec![
            patch(1, "PENDING", "CRITICAL", Some(5)), // device 1 matched → kept
            patch(2, "PENDING", "CRITICAL", Some(5)), // device 2 not in set → dropped
        ];
        let maps = maps();
        let filter = FilterParams {
            node_classes: vec!["LINUX_SERVER".into()],
            ..Default::default()
        };
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &filter,
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].device_id, 1);
    }

    #[test]
    fn install_source_applies_status_override() {
        let d1 = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d1)]);
        let mut p = patch(1, "PENDING", "CRITICAL", None);
        p.status = None;
        let patches = vec![p];
        let maps = maps();
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: Some("INSTALLED"),
                status_filter: None,
            }],
            &FilterParams::default(),
        );
        assert_eq!(rows[0].status, "INSTALLED");
    }

    #[test]
    fn manual_status_matches_pending_filter_and_displays_as_pending() {
        use crate::model::PatchStatus;
        // The "Pending" status maps to NinjaOne's "MANUAL"; a MANUAL patch must pass
        // the Pending filter and render with a "PENDING" label.
        let d = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d)]);
        let maps = maps();
        let patches = vec![patch(1, "MANUAL", "CRITICAL", Some(1))];
        let pending_set = HashSet::from([PatchStatus::Pending.api_value()]);
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: None,
                status_filter: Some(&pending_set),
            }],
            &FilterParams::default(),
        );
        assert_eq!(rows.len(), 1, "a MANUAL patch matches the Pending filter");
        assert_eq!(rows[0].status, "PENDING", "MANUAL renders as PENDING");
    }

    #[test]
    fn failed_filter_keeps_failed_installs_and_drops_installed() {
        use crate::model::PatchStatus;
        // FAILED is an install *result*: it comes from the install-history source
        // (which returns both INSTALLED and FAILED records), narrowed to the
        // requested install statuses. A FAILED-only query must keep the FAILED
        // record and drop the INSTALLED one — the bug was routing FAILED to the
        // current feed, where it never appears, so nothing was returned.
        let d1 = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d1)]);
        let maps = maps();
        let mut failed = patch(1, "FAILED", "CRITICAL", Some(1));
        failed.installed_timestamp = Some((Utc::now() - Duration::days(1)).timestamp() as f64);
        let installed = patch(1, "INSTALLED", "CRITICAL", Some(1));
        let patches = vec![failed, installed];
        let failed_set = HashSet::from([PatchStatus::Failed.api_value()]);
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: Some("INSTALLED"),
                status_filter: Some(&failed_set),
            }],
            &FilterParams::default(),
        );
        assert_eq!(rows.len(), 1, "only the FAILED install record is kept");
        assert_eq!(rows[0].status, "FAILED");
    }

    #[test]
    fn install_filter_falls_back_to_override_for_missing_status() {
        use crate::model::PatchStatus;
        // An install record that omits its own status falls back to the source's
        // override (INSTALLED) for both matching and display, so an INSTALLED query
        // still keeps it.
        let d1 = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d1)]);
        let maps = maps();
        let mut p = patch(1, "INSTALLED", "CRITICAL", Some(1));
        p.status = None;
        let patches = vec![p];
        let installed_set = HashSet::from([PatchStatus::Installed.api_value()]);
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: Some("INSTALLED"),
                status_filter: Some(&installed_set),
            }],
            &FilterParams::default(),
        );
        assert_eq!(rows.len(), 1, "missing status falls back to the override");
        assert_eq!(rows[0].status, "INSTALLED");
    }

    #[test]
    fn compliance_counts_compliant_and_aged_backlog() {
        let d1 = device(1, 10, "Windows Server 2022"); // has pending
        let d2 = device(2, 10, "Windows Server 2019"); // compliant
        let by_id = HashMap::from([(1, &d1), (2, &d2)]);
        let maps = maps();
        let current = vec![
            patch(1, "MANUAL", "CRITICAL", Some(45)), // pending (MANUAL), aged
            patch(1, "APPROVED", "IMPORTANT", Some(2)), // approved, fresh
        ];
        let counts = pending_counts(&current);
        let summaries = build_device_summaries(&[&d1, &d2], &counts, &maps);
        let buckets = build_compliance(&summaries, &current, &by_id, &maps, 30, Utc::now());
        assert_eq!(buckets.len(), 1);
        let b = &buckets[0];
        assert_eq!(b.devices_total, 2);
        assert_eq!(b.devices_compliant, 1);
        assert_eq!(b.pending_critical, 2);
        assert_eq!(b.aged_critical, 1);
        assert!((b.compliance_pct - 50.0).abs() < 1e-9);
    }

    #[test]
    fn compliance_excludes_offline_devices_from_the_denominator() {
        let online = device(1, 10, "Windows Server 2022"); // online, has a pending patch
        let mut offline = device(2, 10, "Windows Server 2019");
        offline.offline = Some(true); // offline → unknown, must not count
        let by_id = HashMap::from([(1, &online), (2, &offline)]);
        let maps = maps();
        let current = vec![patch(1, "MANUAL", "CRITICAL", Some(1))];
        let counts = pending_counts(&current);
        let summaries = build_device_summaries(&[&online, &offline], &counts, &maps);
        let buckets = build_compliance(&summaries, &current, &by_id, &maps, 30, Utc::now());
        assert_eq!(buckets.len(), 1);
        let b = &buckets[0];
        assert_eq!(
            b.devices_total, 1,
            "offline device excluded from denominator"
        );
        assert_eq!(
            b.devices_compliant, 0,
            "the online device has a pending patch"
        );
    }

    #[test]
    fn query_result_serializes_camel_case_for_the_frontend() {
        // web-rs/src/types.rs deserializes the query result with
        // rename_all = "camelCase"; serializing snake_case here breaks decoding
        // with `missing field deviceName`. Guard the IPC contract.
        let d = device(2, 10, "Windows Server 2022");
        let by_id = HashMap::from([(2, &d)]);
        let patches = vec![patch(2, "PENDING", "CRITICAL", Some(1))];
        let maps = maps();
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default(),
        );
        let counts = pending_counts(&patches);
        let devices = build_device_summaries(&[&d], &counts, &maps);
        let compliance = build_compliance(&devices, &patches, &by_id, &maps, 30, Utc::now());
        let result = QueryResult {
            rows,
            devices,
            compliance,
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 1,
            generated_at: "2026-01-01 00:00 UTC".into(),
            data_fetched_at: "2026-01-01 00:00 UTC".into(),
        };

        let json = serde_json::to_string(&result).expect("serialize QueryResult");
        for key in [
            "\"deviceName\"",
            "\"deviceRole\"",
            "\"osName\"",
            "\"patchType\"",
            "\"needsReboot\"",
            "\"pendingCount\"",
            "\"devicesTotal\"",
            "\"generatedAt\"",
            "\"compliancePct\"",
        ] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
        assert!(!json.contains("device_name"), "snake_case leaked: {json}");
    }

    #[test]
    fn query_summary_trims_to_first_page_and_reboot_subset() {
        // Two rows, two devices (one needing reboot). A first page of 1 keeps a
        // single row but reports the true total; only the reboot device is carried.
        let d1 = device(1, 10, "Windows Server 2022"); // id 1 → needs_reboot = false
        let d2 = device(2, 10, "Windows Server 2019"); // id 2 → needs_reboot = true
        let by_id = HashMap::from([(1, &d1), (2, &d2)]);
        let maps = maps();
        let patches = vec![
            patch(1, "MANUAL", "CRITICAL", Some(1)),
            patch(2, "MANUAL", "CRITICAL", Some(1)),
        ];
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default(),
        );
        let counts = pending_counts(&patches);
        let devices = build_device_summaries(&[&d1, &d2], &counts, &maps);
        let compliance = build_compliance(&devices, &patches, &by_id, &maps, 30, Utc::now());
        let result = QueryResult {
            rows,
            devices,
            compliance,
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 2,
            generated_at: "2026-01-01 00:00 UTC".into(),
            data_fetched_at: "2026-01-01 00:00 UTC".into(),
        };

        let summary = QuerySummary::from_result(&result, 1);
        assert_eq!(
            summary.rows.len(),
            1,
            "first page is capped at `first_page`"
        );
        assert_eq!(summary.rows_total, 2, "total reflects the full row set");
        assert_eq!(
            summary.reboot_devices.len(),
            1,
            "only the needs-reboot device is carried"
        );
        assert!(summary.reboot_devices.iter().all(|d| d.needs_reboot));
        assert_eq!(summary.devices_total, 2);

        // The IPC contract is camelCase, same as QueryResult.
        let json = serde_json::to_string(&summary).expect("serialize QuerySummary");
        for key in ["\"rowsTotal\"", "\"rebootDevices\"", "\"devicesTotal\""] {
            assert!(json.contains(key), "missing {key} in {json}");
        }
    }

    #[test]
    fn unmapped_org_and_missing_device_fall_back_to_placeholders() {
        let maps = maps(); // only org 10 ("Contoso") is mapped
        // Device 1 belongs to org 999, which is absent from the lookup map.
        let d1 = device(1, 999, "Windows Server 2022");
        let devices = [d1];
        let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();
        // One patch on the unmapped-org device, one on a device id not in inventory.
        let patches = vec![
            patch(1, "MANUAL", "CRITICAL", Some(1)),
            patch(404, "MANUAL", "CRITICAL", Some(1)),
        ];
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default(),
        );

        assert_eq!(rows.len(), 2);
        let mapped = rows.iter().find(|r| r.device_id == 1).unwrap();
        assert_eq!(
            mapped.organization, "(unknown)",
            "an org id absent from the lookup map renders as (unknown)"
        );
        assert_eq!(mapped.device_name, "srv1");
        let orphan = rows.iter().find(|r| r.device_id == 404).unwrap();
        assert_eq!(
            orphan.device_name, "(unknown)",
            "a patch for a device not in inventory has no resolvable name"
        );
        assert_eq!(orphan.organization, "(unknown)");
    }

    #[test]
    fn empty_inputs_yield_no_rows_or_compliance() {
        let maps = maps();
        let by_id: HashMap<i64, &Device> = HashMap::new();
        let rows = build_rows(&by_id, &maps, &[], &FilterParams::default());
        assert!(rows.is_empty());
        let compliance = build_compliance(&[], &[], &by_id, &maps, 30, Utc::now());
        assert!(compliance.is_empty());
    }

    fn assert_keys_present(value: &serde_json::Value, required: &[&str], what: &str) {
        let obj = value
            .as_object()
            .unwrap_or_else(|| panic!("{what} did not serialize to a JSON object"));
        for key in required {
            assert!(
                obj.contains_key(*key),
                "{what} is missing frontend-required key `{key}` — web-rs/src/types.rs and the \
                 backend struct have drifted (a renamed/dropped field would silently break the UI)"
            );
        }
    }

    /// Pins the IPC wire contract: every key the frontend's mirror DTOs in
    /// `web-rs/src/types.rs` deserialize must be present in the backend's serialized
    /// output. Renaming/removing a backend field the UI reads fails here, before a
    /// user's session silently loses a column, instead of relying on a manual review
    /// of the two independent crates staying in sync.
    #[test]
    fn serialized_shapes_carry_every_frontend_required_key() {
        let d = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d)]);
        let maps = maps();
        let patches = vec![patch(1, "MANUAL", "CRITICAL", Some(1))];
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &patches,
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default(),
        );
        assert_keys_present(
            &serde_json::to_value(&rows[0]).unwrap(),
            &[
                "deviceName",
                "organization",
                "location",
                "deviceRole",
                "osName",
                "patchType",
                "kb",
                "name",
                "severity",
                "status",
                "releaseDate",
                "installedDate",
            ],
            "PatchRow",
        );

        let summaries = build_device_summaries(&[&d], &pending_counts(&patches), &maps);
        assert_keys_present(
            &serde_json::to_value(&summaries[0]).unwrap(),
            &[
                "deviceName",
                "organization",
                "location",
                "deviceRole",
                "osName",
                "pendingCount",
            ],
            "DeviceSummary",
        );

        let compliance = build_compliance(&summaries, &patches, &by_id, &maps, 30, Utc::now());
        assert_keys_present(
            &serde_json::to_value(&compliance[0]).unwrap(),
            &[
                "organization",
                "devicesTotal",
                "devicesCompliant",
                "compliancePct",
                "pendingCritical",
                "agedCritical",
            ],
            "ComplianceBucket",
        );

        let result = QueryResult {
            rows,
            devices: summaries,
            compliance,
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 1,
            generated_at: "2026-01-01 00:00:00 UTC".into(),
            data_fetched_at: "2026-01-01 00:00:00 UTC".into(),
        };
        assert_keys_present(
            &serde_json::to_value(QuerySummary::from_result(&result, 100)).unwrap(),
            &[
                "rows",
                "rowsTotal",
                "rebootDevices",
                "compliance",
                "failures",
                "severityByOrg",
                "ageBuckets",
                "devicesTotal",
                "generatedAt",
                "dataFetchedAt",
            ],
            "QuerySummary",
        );
    }

    fn failed_row(device_id: i64, device: &str, kb: &str, installed_ts: Option<i64>) -> PatchRow {
        PatchRow {
            device_id,
            device_name: device.into(),
            organization: "Contoso".into(),
            location: None,
            device_role: None,
            os_name: None,
            node_class: None,
            needs_reboot: false,
            patch_type: "OS".into(),
            kb: Some(kb.into()),
            name: "Cumulative Update".into(),
            severity: "Critical".into(),
            severity_rank: 5,
            status: "FAILED".into(),
            release_date: None,
            installed_date: installed_ts.map(|_| "2026-01-01 00:00 UTC".into()),
            release_ts: None,
            installed_ts,
        }
    }

    #[test]
    fn build_failures_groups_by_patch_and_counts_distinct_devices() {
        let rows = vec![
            failed_row(1, "srv1", "KB1", Some(100)),
            failed_row(2, "srv2", "KB1", Some(200)), // same patch, second device
            failed_row(1, "srv1", "KB1", Some(50)),  // duplicate device + older
            failed_row(3, "srv3", "KB2", Some(10)),
            // A non-FAILED row in the same set must be ignored.
            PatchRow {
                status: "PENDING".into(),
                ..failed_row(9, "srv9", "KB1", Some(999))
            },
        ];
        let groups = build_failures(&rows);
        assert_eq!(groups.len(), 2, "two distinct failing patches");
        // KB1 fails on 2 distinct devices → sorted ahead of KB2 (1 device).
        let kb1 = &groups[0];
        assert_eq!(kb1.kb.as_deref(), Some("KB1"));
        assert_eq!(kb1.affected_devices, 2, "distinct devices, not records");
        assert_eq!(kb1.latest_failure_ts, Some(200), "most recent failure");
        assert_eq!(kb1.device_names.len(), 2, "full deduped device list");
        assert_eq!(groups[1].affected_devices, 1);
    }

    #[test]
    fn build_severity_by_org_buckets_pending_patches() {
        let d1 = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d1)]);
        let maps = maps();
        let current = vec![
            patch(1, "MANUAL", "CRITICAL", Some(1)),
            patch(1, "APPROVED", "IMPORTANT", Some(1)),
            patch(1, "REJECTED", "CRITICAL", Some(1)), // not pending → ignored
        ];
        let sev = build_severity_by_org(&current, &by_id, &maps);
        assert_eq!(sev.len(), 1);
        assert_eq!(sev[0].organization, "Contoso");
        assert_eq!(sev[0].counts.critical, 1);
        assert_eq!(sev[0].counts.important, 1);
        assert_eq!(sev[0].counts.moderate, 0);
    }

    #[test]
    fn build_age_buckets_route_pending_by_age_and_unknown_to_oldest() {
        let mut unknown = patch(1, "MANUAL", "CRITICAL", Some(5));
        unknown.release_timestamp = None; // can't prove fresh → oldest bucket
        let current = vec![
            patch(1, "MANUAL", "CRITICAL", Some(5)),   // 0-30
            patch(1, "MANUAL", "CRITICAL", Some(200)), // 180+
            unknown,
            patch(1, "INSTALLED", "CRITICAL", Some(5)), // not pending → ignored
        ];
        let buckets = build_age_buckets(&current, Utc::now());
        assert_eq!(buckets.len(), 5, "fixed five-bucket layout");
        assert_eq!(buckets[0].count, 1, "0-30 bucket");
        assert_eq!(
            buckets[4].count, 2,
            "180+ holds the aged and unknown-release"
        );
    }

    #[test]
    fn aggregate_shapes_carry_camel_case_keys() {
        let failures = build_failures(&[failed_row(1, "srv1", "KB1", Some(1))]);
        assert_keys_present(
            &serde_json::to_value(&failures[0]).unwrap(),
            &[
                "patchType",
                "kb",
                "name",
                "severity",
                "severityRank",
                "affectedDevices",
                "deviceNames",
                "latestFailure",
                "latestFailureTs",
            ],
            "FailureGroup",
        );

        let d1 = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d1)]);
        let sev =
            build_severity_by_org(&[patch(1, "MANUAL", "CRITICAL", Some(1))], &by_id, &maps());
        let sev_json = serde_json::to_value(&sev[0]).unwrap();
        assert_keys_present(&sev_json, &["organization", "counts"], "OrgSeverity");
        assert_keys_present(
            &sev_json["counts"],
            &[
                "critical",
                "important",
                "moderate",
                "low",
                "optional",
                "unknown",
            ],
            "SeverityCounts",
        );

        let buckets = build_age_buckets(&[patch(1, "MANUAL", "CRITICAL", Some(1))], Utc::now());
        assert_keys_present(
            &serde_json::to_value(&buckets[0]).unwrap(),
            &["label", "count"],
            "AgeBucket",
        );
    }
}
