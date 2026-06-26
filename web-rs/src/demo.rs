//! Sample-data builder + client-side filtering for the app's demo mode.
//!
//! Produces the org/location/role/OS-type lookups and, via [`filtered_result`], a
//! [`QueryResult`] over invented orgs/devices/patches — so browser/web mode (the
//! GitHub Pages demo, where there is no Tauri backend) can render and *filter*
//! populated tables with no NinjaOne account, sign-in, or real fleet data. Like the
//! real app, the results stay empty until the user presses **Run query**, which
//! routes to `AppState::run_demo_query` → `filtered_result`.
//!
//! In a real deployment the backend applies the filters (server-side `df` for
//! identity/class facets, client-side for the rest) against live NinjaOne data.
//! Here [`filtered_result`] mirrors that *display* filtering over the sample rows so
//! the demo's controls behave like the real thing. The compliance / needs-reboot
//! rollups are backend computations, so they stay representative (narrowed only by
//! the organization facet).
//!
//! It is pure data — no `js_sys`, no IPC — so it compiles and unit-tests on the host
//! target via `just web-test`, like the helpers in [`crate::app::util`].

use crate::types::QueryResult;
use crate::types::{
    AgeBucket, ComplianceBucket, DeviceSummary, FailureGroup, FilterParams, Location, NodeClass,
    OrgSeverity, Organization, PatchRow, Role, SeverityCounts,
};

/// Wall-clock label shown in the results summary. Fixed (not "now") so the build
/// stays deterministic and host-testable — it reads as a representative snapshot.
const GENERATED_AT: &str = "2026-06-26 14:32:08 UTC";
/// Reference "now" for the demo's relative date filters (the epoch of
/// `GENERATED_AT`), so "last N days" is measured against the sample's snapshot date
/// rather than the real clock — otherwise every window would be empty.
const SAMPLE_NOW_EPOCH: i64 = 1_782_484_328; // 2026-06-26 14:32:08 UTC

// Identity tables. The IDs are arbitrary but stable; they exist so the
// Organization / Location / Device Role facets (which select by id) can filter the
// sample rows, and so the dropdowns can be populated from the same source.
const ORGS: [(i64, &str); 3] = [
    (1, "Contoso Ltd"),
    (2, "Northwind Traders"),
    (3, "Fabrikam Inc"),
];
const LOCATIONS: [(i64, i64, &str); 5] = [
    (11, 1, "HQ — Seattle"),
    (12, 1, "Datacenter A"),
    (21, 2, "Datacenter B"),
    (22, 2, "Branch — Austin"),
    (31, 3, "Cloud — us-east-1"),
];
const ROLES: [(i64, &str); 6] = [
    (101, "Domain Controller"),
    (102, "Application Server"),
    (103, "Web Server"),
    (104, "Workstation"),
    (105, "Database Server"),
    (106, "File Server"),
];

/// A sample patch row plus the identity keys the device facets filter on. `row` is
/// the display projection sent to the table; the keys never reach the UI.
struct DemoRow {
    org_id: i64,
    location_id: i64,
    role_id: i64,
    node_class: &'static str,
    row: PatchRow,
}

fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

fn org_id_of(name: &str) -> i64 {
    ORGS.iter()
        .find(|(_, n)| *n == name)
        .map_or(0, |(id, _)| *id)
}

fn location_id_of(org_id: i64, name: &str) -> i64 {
    LOCATIONS
        .iter()
        .find(|(_, o, n)| *o == org_id && *n == name)
        .map_or(0, |(id, ..)| *id)
}

fn role_id_of(name: &str) -> i64 {
    ROLES
        .iter()
        .find(|(_, n)| *n == name)
        .map_or(0, |(id, _)| *id)
}

fn org_name(id: i64) -> Option<&'static str> {
    ORGS.iter().find(|(i, _)| *i == id).map(|(_, n)| *n)
}

/// One sample row. Arg order mirrors the Patches table columns, then the node class.
/// org/location/role IDs are resolved from the names so the data table stays readable.
#[allow(clippy::too_many_arguments)]
fn row(
    org: &str,
    location: &str,
    role: &str,
    device: &str,
    os: &str,
    patch_type: &str,
    kb: &str,
    name: &str,
    severity: &str,
    status: &str,
    release_date: &str,
    installed_date: &str,
    node_class: &'static str,
) -> DemoRow {
    let org_id = org_id_of(org);
    DemoRow {
        org_id,
        location_id: location_id_of(org_id, location),
        role_id: role_id_of(role),
        node_class,
        row: PatchRow {
            device_name: device.to_string(),
            organization: org.to_string(),
            location: opt(location),
            device_role: opt(role),
            os_name: opt(os),
            patch_type: patch_type.to_string(),
            kb: opt(kb),
            name: name.to_string(),
            // Severity renders in title case (see `app::util::sev_class`); status is
            // upper-case (`status_class`). Filtering compares case-insensitively.
            severity: severity.to_string(),
            status: status.to_string(),
            release_date: opt(release_date),
            installed_date: opt(installed_date),
        },
    }
}

/// The OS-type facet options, mirroring the backend `list_node_classes` so the
/// Filters panel looks complete in browser mode (where the IPC lookup is absent).
pub fn sample_node_classes() -> Vec<NodeClass> {
    [
        ("WINDOWS_SERVER", "Windows Server"),
        ("WINDOWS_WORKSTATION", "Windows Workstation"),
        ("MAC_SERVER", "macOS Server"),
        ("MAC", "macOS"),
        ("LINUX_SERVER", "Linux Server"),
        ("LINUX_WORKSTATION", "Linux Workstation"),
    ]
    .into_iter()
    .map(|(value, label)| NodeClass {
        value: value.to_string(),
        label: label.to_string(),
    })
    .collect()
}

/// Organizations for the demo's Organization dropdown.
pub fn sample_orgs() -> Vec<Organization> {
    ORGS.iter()
        .map(|(id, name)| Organization {
            id: *id,
            name: name.to_string(),
        })
        .collect()
}

/// Device roles for the demo's Device Role dropdown.
pub fn sample_roles() -> Vec<Role> {
    ROLES
        .iter()
        .map(|(id, name)| Role {
            id: *id,
            name: name.to_string(),
        })
        .collect()
}

/// Locations belonging to `org_id` for the demo's Location dropdown (mirrors the
/// backend's org-scoped lookup).
pub fn sample_locations(org_id: i64) -> Vec<Location> {
    LOCATIONS
        .iter()
        .filter(|(_, o, _)| *o == org_id)
        .map(|(id, _, name)| Location {
            id: *id,
            name: name.to_string(),
        })
        .collect()
}

#[rustfmt::skip]
fn demo_rows() -> Vec<DemoRow> {
    // (org, location, role, device, os, type, kb, name, severity, status, released, installed, node_class)
    vec![
        // --- Contoso Ltd ---
        row("Contoso Ltd", "HQ — Seattle", "Domain Controller", "SEA-DC01", "Windows Server 2022", "OS", "KB5062553", "2026-06 Cumulative Update for Windows Server 2022 (KB5062553)", "Critical", "PENDING", "2026-06-09", "", "WINDOWS_SERVER"),
        row("Contoso Ltd", "HQ — Seattle", "Web Server", "SEA-WEB01", "Windows Server 2022", "OS", "KB5062553", "2026-06 Cumulative Update for Windows Server 2022 (KB5062553)", "Critical", "PENDING", "2026-06-09", "", "WINDOWS_SERVER"),
        row("Contoso Ltd", "Datacenter A", "Application Server", "DCA-APP01", "Windows Server 2019", "OS", "KB5062561", "2026-06 Cumulative Update for Windows Server 2019 (KB5062561)", "Important", "APPROVED", "2026-06-09", "", "WINDOWS_SERVER"),
        row("Contoso Ltd", "Datacenter A", "Application Server", "DCA-APP01", "Windows Server 2019", "Software", "", "Adobe Acrobat Reader 26.001.20512", "Critical", "PENDING", "2026-06-12", "", "WINDOWS_SERVER"),
        row("Contoso Ltd", "HQ — Seattle", "Workstation", "SEA-WKS-1042", "Windows 11 Pro", "OS", "KB5062554", "2026-06 Cumulative Update for Windows 11 24H2 (KB5062554)", "Critical", "INSTALLED", "2026-06-10", "2026-06-12", "WINDOWS_WORKSTATION"),
        row("Contoso Ltd", "HQ — Seattle", "Workstation", "SEA-WKS-1042", "Windows 11 Pro", "Software", "", "Google Chrome 137.0.7151.69", "Important", "INSTALLED", "2026-06-11", "2026-06-12", "WINDOWS_WORKSTATION"),
        row("Contoso Ltd", "HQ — Seattle", "Workstation", "SEA-WKS-1077", "Windows 11 Pro", "Software", "", "Microsoft Edge 137.0.3296.62", "Low", "PENDING", "2026-06-11", "", "WINDOWS_WORKSTATION"),
        row("Contoso Ltd", "Datacenter A", "Web Server", "DCA-WEB02", "Windows Server 2022", "OS", "KB5062553", "2026-06 Cumulative Update for Windows Server 2022 (KB5062553)", "Critical", "FAILED", "2026-06-09", "2026-06-13", "WINDOWS_SERVER"),
        // --- Northwind Traders ---
        row("Northwind Traders", "Datacenter B", "Database Server", "NW-SQL01", "Windows Server 2022", "OS", "KB5062553", "2026-06 Cumulative Update for Windows Server 2022 (KB5062553)", "Critical", "PENDING", "2026-06-09", "", "WINDOWS_SERVER"),
        row("Northwind Traders", "Datacenter B", "Database Server", "NW-SQL01", "Windows Server 2022", "Software", "", "7-Zip 24.09", "Moderate", "PENDING", "2026-06-05", "", "WINDOWS_SERVER"),
        row("Northwind Traders", "Datacenter B", "File Server", "NW-FILE01", "Windows Server 2019", "OS", "KB5062561", "2026-06 Cumulative Update for Windows Server 2019 (KB5062561)", "Important", "INSTALLED", "2026-06-09", "2026-06-11", "WINDOWS_SERVER"),
        row("Northwind Traders", "Branch — Austin", "Workstation", "ATX-WKS-2207", "Windows 10 Pro", "OS", "KB5062560", "2026-06 Cumulative Update for Windows 10 22H2 (KB5062560)", "Important", "PENDING", "2026-06-10", "", "WINDOWS_WORKSTATION"),
        row("Northwind Traders", "Branch — Austin", "Workstation", "ATX-WKS-2207", "Windows 10 Pro", "Software", "", "Mozilla Firefox 140.0", "Moderate", "REJECTED", "2026-06-10", "", "WINDOWS_WORKSTATION"),
        row("Northwind Traders", "Branch — Austin", "Workstation", "ATX-MAC-0099", "macOS 15.5 Sequoia", "OS", "", "macOS 15.5 Security Update 2026-003", "Important", "PENDING", "2026-06-09", "", "MAC"),
        row("Northwind Traders", "Branch — Austin", "Workstation", "ATX-MAC-0099", "macOS 15.5 Sequoia", "Software", "", "Google Chrome 137.0.7151.69", "Important", "INSTALLED", "2026-06-11", "2026-06-12", "MAC"),
        row("Northwind Traders", "Datacenter B", "Application Server", "NW-APP05", "Windows Server 2022", "Software", "", "Notepad++ 8.7.6", "Low", "APPROVED", "2026-06-03", "", "WINDOWS_SERVER"),
        // --- Fabrikam Inc ---
        row("Fabrikam Inc", "Cloud — us-east-1", "Application Server", "FAB-LNX-APP3", "Ubuntu 22.04 LTS", "Software", "", "OpenSSL 3.0.16 (libssl)", "Critical", "PENDING", "2026-06-08", "", "LINUX_SERVER"),
        row("Fabrikam Inc", "Cloud — us-east-1", "Application Server", "FAB-LNX-APP3", "Ubuntu 22.04 LTS", "Software", "", "Docker Engine 28.1.1", "Important", "INSTALLED", "2026-06-06", "2026-06-10", "LINUX_SERVER"),
        row("Fabrikam Inc", "Cloud — us-east-1", "Web Server", "FAB-LNX-WEB1", "Ubuntu 22.04 LTS", "Software", "", "nginx 1.27.5", "Important", "PENDING", "2026-06-07", "", "LINUX_SERVER"),
        row("Fabrikam Inc", "Cloud — us-east-1", "Web Server", "FAB-LNX-WEB1", "Ubuntu 22.04 LTS", "Software", "", "OpenSSL 3.0.16 (libssl)", "Critical", "FAILED", "2026-06-08", "2026-06-11", "LINUX_SERVER"),
        row("Fabrikam Inc", "Cloud — us-east-1", "Workstation", "FAB-WKS-3310", "Windows 11 Pro", "OS", "KB5062554", "2026-06 Cumulative Update for Windows 11 24H2 (KB5062554)", "Critical", "PENDING", "2026-06-10", "", "WINDOWS_WORKSTATION"),
        row("Fabrikam Inc", "Cloud — us-east-1", "Workstation", "FAB-WKS-3310", "Windows 11 Pro", "Software", "", "Microsoft Edge 137.0.3296.62", "Low", "INSTALLED", "2026-06-11", "2026-06-12", "WINDOWS_WORKSTATION"),
    ]
}

// Fixed, representative rollups. They are backend computations in a real deployment,
// so the demo keeps them static and only narrows them by the organization facet.
fn sample_compliance() -> Vec<ComplianceBucket> {
    vec![
        ComplianceBucket {
            organization: "Contoso Ltd".to_string(),
            devices_total: 18,
            devices_compliant: 12,
            compliance_pct: 66.7,
            pending_critical: 5,
            aged_critical: 2,
        },
        ComplianceBucket {
            organization: "Northwind Traders".to_string(),
            devices_total: 14,
            devices_compliant: 11,
            compliance_pct: 78.6,
            pending_critical: 3,
            aged_critical: 1,
        },
        ComplianceBucket {
            organization: "Fabrikam Inc".to_string(),
            devices_total: 10,
            devices_compliant: 9,
            compliance_pct: 90.0,
            pending_critical: 1,
            aged_critical: 0,
        },
    ]
}

fn sample_reboot() -> Vec<DeviceSummary> {
    vec![
        reboot(
            "Contoso Ltd",
            "HQ — Seattle",
            "Domain Controller",
            "SEA-DC01",
            "Windows Server 2022",
            3,
        ),
        reboot(
            "Contoso Ltd",
            "Datacenter A",
            "Web Server",
            "DCA-WEB02",
            "Windows Server 2022",
            2,
        ),
        reboot(
            "Northwind Traders",
            "Datacenter B",
            "Database Server",
            "NW-SQL01",
            "Windows Server 2022",
            2,
        ),
        reboot(
            "Northwind Traders",
            "Branch — Austin",
            "Workstation",
            "ATX-WKS-2207",
            "Windows 10 Pro",
            4,
        ),
        reboot(
            "Fabrikam Inc",
            "Cloud — us-east-1",
            "Web Server",
            "FAB-LNX-WEB1",
            "Ubuntu 22.04 LTS",
            1,
        ),
    ]
}

fn reboot(
    org: &str,
    location: &str,
    role: &str,
    device: &str,
    os: &str,
    pending: usize,
) -> DeviceSummary {
    DeviceSummary {
        device_name: device.to_string(),
        organization: org.to_string(),
        location: opt(location),
        device_role: opt(role),
        os_name: opt(os),
        pending_count: pending,
    }
}

/// Groups the demo's FAILED display rows by patch (KB + name) — the demo mirror of
/// the backend `build_failures`, so the Top-failures tab reacts to the status facet
/// (it stays empty until FAILED is selected, exactly like the real app).
fn demo_failures(rows: &[PatchRow]) -> Vec<FailureGroup> {
    let mut groups: Vec<FailureGroup> = Vec::new();
    for r in rows
        .iter()
        .filter(|r| r.status.eq_ignore_ascii_case("FAILED"))
    {
        match groups.iter_mut().find(|g| g.kb == r.kb && g.name == r.name) {
            Some(g) => {
                // Count each device once; keep the latest (max YYYY-MM-DD) failure.
                if !g.sample_devices.contains(&r.device_name) {
                    g.affected_devices += 1;
                    g.sample_devices.push(r.device_name.clone());
                }
                if r.installed_date.as_deref() > g.latest_failure.as_deref() {
                    g.latest_failure = r.installed_date.clone();
                }
            }
            None => groups.push(FailureGroup {
                kb: r.kb.clone(),
                name: r.name.clone(),
                severity: r.severity.clone(),
                affected_devices: 1,
                sample_devices: vec![r.device_name.clone()],
                latest_failure: r.installed_date.clone(),
            }),
        }
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.affected_devices));
    groups
}

/// Representative per-org pending-patch severity breakdown for the dashboard. Like
/// `sample_compliance`, it's a backend computation in a real deployment, so the demo
/// keeps it static and narrows it by the organization facet.
fn sample_severity_by_org() -> Vec<OrgSeverity> {
    fn org(name: &str, c: usize, i: usize, m: usize, l: usize) -> OrgSeverity {
        OrgSeverity {
            organization: name.to_string(),
            counts: SeverityCounts {
                critical: c,
                important: i,
                moderate: m,
                low: l,
                optional: 0,
                unknown: 0,
            },
        }
    }
    vec![
        org("Contoso Ltd", 5, 3, 1, 2),
        org("Northwind Traders", 3, 2, 2, 1),
        org("Fabrikam Inc", 1, 1, 0, 1),
    ]
}

/// Representative fleet-wide pending-patch age histogram. The labels match the
/// backend's fixed buckets (`build_age_buckets`), oldest last.
fn sample_age_buckets() -> Vec<AgeBucket> {
    [
        ("0-30 days", 13),
        ("31-60 days", 6),
        ("61-90 days", 4),
        ("91-180 days", 3),
        ("180+ days", 2),
    ]
    .into_iter()
    .map(|(label, count)| AgeBucket {
        label: label.to_string(),
        count,
    })
    .collect()
}

/// The sample filtered like a real query: device facets (org/location/role/OS-type/
/// OS-name) and patch facets (type/status/severity/search/release-window/install-
/// window) applied to the rows, with the row count recomputed. Compliance / reboot
/// stay representative, narrowed only by the organization facet for consistency.
pub fn filtered_result(
    filter: &FilterParams,
    patch_type: &str,
    statuses: &[String],
    install_after_days: Option<i64>,
) -> QueryResult {
    let rows = demo_rows()
        .into_iter()
        .filter(|d| device_matches(d, filter))
        .filter(|d| patch_matches(&d.row, filter, patch_type, statuses, install_after_days))
        .map(|d| d.row)
        .collect();
    assemble(rows, filter.organization_id)
}

/// Builds a `QueryResult` from already-filtered display rows, narrowing the rollups
/// to `org_filter` (the organization facet) when one is set.
fn assemble(rows: Vec<PatchRow>, org_filter: Option<i64>) -> QueryResult {
    // Failures derive from the already-filtered rows, so the tab reacts to filters.
    let failures = demo_failures(&rows);
    let (compliance, reboot_devices, severity_by_org, devices_total) =
        match org_filter.and_then(org_name) {
            Some(name) => {
                let compliance: Vec<_> = sample_compliance()
                    .into_iter()
                    .filter(|b| b.organization == name)
                    .collect();
                let reboot_devices = sample_reboot()
                    .into_iter()
                    .filter(|d| d.organization == name)
                    .collect();
                let severity_by_org = sample_severity_by_org()
                    .into_iter()
                    .filter(|o| o.organization == name)
                    .collect();
                let devices_total = compliance.iter().map(|b| b.devices_total).sum();
                (compliance, reboot_devices, severity_by_org, devices_total)
            }
            None => {
                let compliance = sample_compliance();
                let devices_total = compliance.iter().map(|b| b.devices_total).sum();
                (
                    compliance,
                    sample_reboot(),
                    sample_severity_by_org(),
                    devices_total,
                )
            }
        };
    QueryResult {
        rows_total: rows.len(),
        rows,
        reboot_devices,
        compliance,
        failures,
        severity_by_org,
        age_buckets: sample_age_buckets(),
        devices_total,
        generated_at: GENERATED_AT.to_string(),
    }
}

fn device_matches(d: &DemoRow, f: &FilterParams) -> bool {
    f.organization_id.is_none_or(|id| d.org_id == id)
        && f.location_id.is_none_or(|id| d.location_id == id)
        && f.role_id.is_none_or(|id| d.role_id == id)
        && (f.node_classes.is_empty()
            || f.node_classes
                .iter()
                .any(|c| c.eq_ignore_ascii_case(d.node_class)))
        && f.os_name_contains
            .as_deref()
            .is_none_or(|q| contains_ci(d.row.os_name.as_deref().unwrap_or(""), q))
}

fn patch_matches(
    row: &PatchRow,
    f: &FilterParams,
    patch_type: &str,
    statuses: &[String],
    install_after_days: Option<i64>,
) -> bool {
    type_matches(patch_type, &row.patch_type)
        && statuses.iter().any(|s| s.eq_ignore_ascii_case(&row.status))
        && (f.severities.is_empty()
            || f.severities
                .iter()
                .any(|s| s.eq_ignore_ascii_case(&row.severity)))
        && f.search.as_deref().is_none_or(|q| search_matches(row, q))
        && release_in_window(row, f)
        && install_in_window(row, install_after_days)
}

fn type_matches(patch_type: &str, row_type: &str) -> bool {
    match patch_type {
        "OS" | "SOFTWARE" => row_type.eq_ignore_ascii_case(patch_type),
        _ => true, // "ALL" or anything unexpected
    }
}

fn search_matches(row: &PatchRow, query: &str) -> bool {
    contains_ci(&row.name, query) || contains_ci(row.kb.as_deref().unwrap_or(""), query)
}

fn release_in_window(row: &PatchRow, f: &FilterParams) -> bool {
    let Some(released) = row.release_date.as_deref().and_then(ymd_to_epoch) else {
        // No release date can't satisfy a date window; pass only when none is set.
        return f.release_within_days.is_none()
            && f.release_after.is_none()
            && f.release_before.is_none();
    };
    if let Some(days) = f.release_within_days
        && released < SAMPLE_NOW_EPOCH - days * 86_400
    {
        return false;
    }
    if let Some(after) = f.release_after
        && released < after
    {
        return false;
    }
    if let Some(before) = f.release_before
        && released > before
    {
        return false;
    }
    true
}

fn install_in_window(row: &PatchRow, install_after_days: Option<i64>) -> bool {
    // The window only constrains install-history rows (INSTALLED / FAILED).
    let is_history =
        row.status.eq_ignore_ascii_case("INSTALLED") || row.status.eq_ignore_ascii_case("FAILED");
    let Some(days) = install_after_days.filter(|_| is_history) else {
        return true;
    };
    match row.installed_date.as_deref().and_then(ymd_to_epoch) {
        Some(installed) => installed >= SAMPLE_NOW_EPOCH - days * 86_400,
        None => false,
    }
}

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack
        .to_lowercase()
        .contains(&needle.trim().to_lowercase())
}

/// Parses a `YYYY-MM-DD` date to Unix seconds at UTC midnight (Howard Hinnant's
/// civil-from-days algorithm), or `None`. Pure, so the date filters host-test.
fn ymd_to_epoch(s: &str) -> Option<i64> {
    let mut parts = s.trim().split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400)
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> FilterParams {
        FilterParams::default()
    }

    fn all_statuses() -> Vec<String> {
        ["PENDING", "APPROVED", "REJECTED", "INSTALLED", "FAILED"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    #[test]
    fn unfiltered_filter_keeps_every_row_and_is_consistent() {
        // ALL type + every status + default (empty) facets keeps every row.
        let r = filtered_result(&filter(), "ALL", &all_statuses(), Some(3650));
        assert_eq!(r.rows_total, demo_rows().len());
        assert_eq!(r.rows_total, r.rows.len());
        assert!(!r.compliance.is_empty());
        assert!(!r.reboot_devices.is_empty());
        let summed: usize = r.compliance.iter().map(|b| b.devices_total).sum();
        assert_eq!(r.devices_total, summed);
        assert!(
            r.compliance
                .iter()
                .all(|b| b.devices_compliant <= b.devices_total)
        );
    }

    #[test]
    fn ymd_to_epoch_matches_known_dates() {
        assert_eq!(ymd_to_epoch("2026-01-01"), Some(1_767_225_600));
        assert_eq!(ymd_to_epoch("2026-06-26"), Some(1_782_432_000));
        assert_eq!(ymd_to_epoch("nonsense"), None);
        assert_eq!(ymd_to_epoch("2026-13-01"), None);
    }

    #[test]
    fn status_facet_narrows_rows() {
        let only_failed = filtered_result(&filter(), "ALL", &["FAILED".to_string()], Some(3650));
        assert!(only_failed.rows_total > 0);
        assert!(only_failed.rows.iter().all(|r| r.status == "FAILED"));
    }

    #[test]
    fn type_facet_keeps_only_os_patches() {
        let os = filtered_result(&filter(), "OS", &all_statuses(), Some(3650));
        assert!(os.rows_total > 0);
        assert!(os.rows.iter().all(|r| r.patch_type == "OS"));
    }

    #[test]
    fn severity_facet_filters_by_selected_levels() {
        let f = FilterParams {
            severities: vec!["CRITICAL".to_string()],
            ..FilterParams::default()
        };
        let r = filtered_result(&f, "ALL", &all_statuses(), Some(3650));
        assert!(r.rows_total > 0);
        assert!(r.rows.iter().all(|r| r.severity == "Critical"));
    }

    #[test]
    fn org_facet_filters_rows_and_rollups() {
        let f = FilterParams {
            organization_id: Some(1), // Contoso Ltd
            ..FilterParams::default()
        };
        let r = filtered_result(&f, "ALL", &all_statuses(), Some(3650));
        assert!(r.rows.iter().all(|r| r.organization == "Contoso Ltd"));
        assert_eq!(r.compliance.len(), 1);
        assert_eq!(r.compliance[0].organization, "Contoso Ltd");
        assert!(
            r.reboot_devices
                .iter()
                .all(|d| d.organization == "Contoso Ltd")
        );
        assert_eq!(r.severity_by_org.len(), 1);
        assert_eq!(r.severity_by_org[0].organization, "Contoso Ltd");
    }

    #[test]
    fn failed_query_populates_demo_failure_rollup() {
        let r = filtered_result(&filter(), "ALL", &["FAILED".to_string()], Some(3650));
        assert!(
            !r.failures.is_empty(),
            "FAILED rows feed the failure rollup"
        );
        assert!(r.failures.iter().all(|f| f.affected_devices >= 1));
        // Sorted by affected-device count, descending.
        assert!(
            r.failures
                .windows(2)
                .all(|w| w[0].affected_devices >= w[1].affected_devices)
        );
    }

    #[test]
    fn pending_only_query_has_no_failures() {
        let r = filtered_result(&filter(), "ALL", &["PENDING".to_string()], Some(3650));
        assert!(
            r.failures.is_empty(),
            "no FAILED rows selected → empty failure rollup, like the real app"
        );
    }

    #[test]
    fn dashboard_rollups_are_always_populated() {
        let r = filtered_result(&filter(), "ALL", &all_statuses(), Some(3650));
        assert!(!r.severity_by_org.is_empty());
        assert_eq!(r.age_buckets.len(), 5, "fixed five-bucket histogram");
    }

    #[test]
    fn node_class_facet_filters_by_os_type() {
        let f = FilterParams {
            node_classes: vec!["LINUX_SERVER".to_string()],
            ..FilterParams::default()
        };
        let r = filtered_result(&f, "ALL", &all_statuses(), Some(3650));
        assert!(r.rows_total > 0);
        assert!(
            r.rows
                .iter()
                .all(|r| r.os_name.as_deref() == Some("Ubuntu 22.04 LTS"))
        );
    }

    #[test]
    fn search_matches_kb_or_name() {
        let f = FilterParams {
            search: Some("openssl".to_string()),
            ..FilterParams::default()
        };
        let r = filtered_result(&f, "ALL", &all_statuses(), Some(3650));
        assert!(r.rows_total > 0);
        assert!(
            r.rows
                .iter()
                .all(|r| r.name.to_lowercase().contains("openssl"))
        );
    }

    #[test]
    fn lookups_expose_ids_and_scoped_locations() {
        assert_eq!(sample_orgs().len(), 3);
        assert_eq!(sample_roles().len(), 6);
        assert_eq!(sample_node_classes().len(), 6);
        // Contoso (id 1) has two locations; an unknown org has none.
        assert_eq!(sample_locations(1).len(), 2);
        assert!(sample_locations(999).is_empty());
    }
}
