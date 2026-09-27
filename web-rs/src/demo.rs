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

use std::collections::{BTreeMap, BTreeSet};

use crate::app::util::{
    date_to_epoch, median, nearest_rank, product_display_name, severity_rank, sla_days_for,
    strip_version_token,
};
use crate::types::QueryResult;
use crate::types::{
    AgeBucket, ApprovalBacklog, ComplianceBucket, DeviceBacklog, DeviceBacklogList, DeviceDetail,
    DeviceSummary, FailureGroup, FilterParams, GroupBy, InstallLatency, Location, NodeClass,
    OrgSeverity, Organization, OsCompliance, PatchFamilies, PatchGroup, PatchRow, Role,
    RollupScope, SeverityCounts, SlaBySeverity, SlaPolicy, StuckDevice, TimeToInstall,
};
use crate::types::{ChangeItem, RunChanges};

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

/// Stable synthetic device id derived from the device name (FNV-1a, 32-bit).
///
/// Selection is device-keyed, so every sample row for a given host must resolve to
/// the same id — otherwise the demo's checkboxes would treat each row as its own
/// device, and "3 devices selected" would really mean three rows on one machine.
fn device_id_of(name: &str) -> i64 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in name.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    // Keep it positive and small enough to read in a debugger.
    i64::from(hash & 0x7fff_ffff)
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
    first_seen_date: &str,
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
            device_id: device_id_of(device),
            device_name: device.to_string(),
            organization: org.to_string(),
            location: opt(location),
            device_role: opt(role),
            os_name: opt(os),
            // The sample fleet is all online; the demo's action controls are
            // disabled anyway, so this never changes what it shows.
            offline: false,
            // Set on the row, the way the backend sets it, so `group_rows` reads it
            // off the row rather than looking the device name up in a side table.
            // A name is not an identity, and that lookup was also the reason a demo
            // group header could disagree with the backend's.
            needs_reboot: REBOOT_DEVICE_NAMES.contains(&device),
            patch_type: patch_type.to_string(),
            kb: opt(kb),
            name: name.to_string(),
            // Severity renders in title case (see `app::util::sev_class`); status is
            // upper-case (`status_class`). Filtering compares case-insensitively.
            severity: severity.to_string(),
            status: status.to_string(),
            first_seen_date: opt(first_seen_date),
            installed_date: opt(installed_date),
            // NinjaOne sends a uuid per third-party product; the sample derives a
            // stable stand-in from the version-free title, so every version of one
            // product shares it and the By product view has something to fold.
            product_identifier: patch_type.eq_ignore_ascii_case("software").then(|| {
                format!(
                    "demo-{}",
                    strip_version_token(name)
                        .to_ascii_lowercase()
                        .replace(' ', "-")
                )
            }),
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

/// Locations belonging to any of `org_ids` for the demo's Location picker (mirrors
/// the backend's `list_locations`, where an empty list means every organization).
pub fn sample_locations(org_ids: &[i64]) -> Vec<Location> {
    LOCATIONS
        .iter()
        .filter(|(_, o, _)| org_ids.is_empty() || org_ids.contains(o))
        .map(|(id, org, name)| Location {
            id: *id,
            name: name.to_string(),
            organization_id: Some(*org),
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
        row("Northwind Traders", "Branch — Austin", "Workstation", "ATX-MAC-0099", "macOS 15.5 Sequoia", "Software", "", "Google Chrome 137.0.7151.104", "Important", "INSTALLED", "2026-06-11", "2026-06-12", "MAC"),
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
            awaiting_approval: 6,
            approved_not_installed: 7,
        },
        ComplianceBucket {
            organization: "Northwind Traders".to_string(),
            devices_total: 14,
            devices_compliant: 11,
            compliance_pct: 78.6,
            pending_critical: 3,
            aged_critical: 1,
            awaiting_approval: 4,
            approved_not_installed: 3,
        },
        ComplianceBucket {
            organization: "Fabrikam Inc".to_string(),
            devices_total: 10,
            devices_compliant: 9,
            compliance_pct: 90.0,
            pending_critical: 1,
            aged_critical: 0,
            awaiting_approval: 2,
            approved_not_installed: 1,
        },
    ]
}

/// The approval backlog for the demo: totals summed from the (org-narrowed)
/// compliance buckets, as the backend's equal the compliance columns' sums, and a
/// fixed stuck-device list narrowed by the same organization facet.
fn sample_approvals(
    compliance: &[ComplianceBucket],
    keep: &dyn Fn(&str) -> bool,
) -> ApprovalBacklog {
    let stuck: Vec<StuckDevice> = [
        ("SEA-WKS-1187", "Contoso Ltd", 4, "2026-04-14 09:20 UTC"),
        ("NW-APP02", "Northwind Traders", 2, "2026-05-02 17:05 UTC"),
        ("SEA-FILE02", "Contoso Ltd", 1, "2026-05-19 03:40 UTC"),
    ]
    .into_iter()
    .filter(|(_, org, _, _)| keep(org))
    .map(|(name, org, patches, seen)| StuckDevice {
        device_name: name.to_string(),
        organization: org.to_string(),
        patches,
        oldest_first_seen: Some(seen.to_string()),
    })
    .collect();
    ApprovalBacklog {
        awaiting_approval: compliance.iter().map(|b| b.awaiting_approval).sum(),
        approved_not_installed: compliance.iter().map(|b| b.approved_not_installed).sum(),
        stuck_after_days: 30,
        stuck_patches: stuck.iter().map(|d| d.patches).sum(),
        stuck_devices_total: stuck.len(),
        stuck_devices: stuck,
    }
}

/// Per-OS compliance for the "Compliance by OS" section. Totals match the fleet size
/// in `sample_compliance` (42 devices). Like the other rollups it's a backend
/// computation in a real deployment; the demo keeps it static (and, unlike the
/// per-org rollups, does not narrow it by organization — the sample carries no
/// per-org OS split).
/// Per-OS compliance over the rows actually on screen. A device counts as
/// compliant when none of its visible rows is a pending patch, mirroring the
/// backend's `build_compliance_by_os`, which likewise derives from the scoped set
/// rather than from a fixed table.
fn scoped_compliance_by_os(rows: &[PatchRow]) -> Vec<OsCompliance> {
    // os -> (all devices, devices with a pending row, pending critical/important,
    // awaiting approval, approved-not-installed)
    type Acc = (BTreeSet<i64>, BTreeSet<i64>, usize, usize, usize);
    let mut by_os: BTreeMap<String, Acc> = BTreeMap::new();
    for r in rows {
        let os = r.os_name.clone().unwrap_or_else(|| "(unknown)".to_string());
        let e = by_os.entry(os).or_default();
        e.0.insert(r.device_id);
        if r.status == "PENDING" {
            e.1.insert(r.device_id);
            e.3 += 1;
            if matches!(r.severity.as_str(), "Critical" | "Important") {
                e.2 += 1;
            }
        }
        if r.status == "APPROVED" {
            e.4 += 1;
        }
    }
    by_os
        .into_iter()
        .map(|(os, (devices, pending, critical, awaiting, approved))| {
            let total = devices.len();
            let compliant = total - pending.len();
            OsCompliance {
                os,
                devices_total: total,
                devices_compliant: compliant,
                compliance_pct: if total == 0 {
                    100.0
                } else {
                    compliant as f64 / total as f64 * 100.0
                },
                pending_critical: critical,
                // The sample carries no SLA breach detail; the real backend
                // computes this from first-seen age.
                aged_critical: 0,
                awaiting_approval: awaiting,
                approved_not_installed: approved,
            }
        })
        .collect()
}

/// Devices the sample says need a reboot. Single-sourced so the Needs Reboot tab and
/// the Patches tab's group headers cannot disagree about which machines they are —
/// the backend derives both from one device inventory, which the demo has none of.
const REBOOT_DEVICE_NAMES: [&str; 5] = [
    "SEA-DC01",
    "DCA-WEB02",
    "NW-SQL01",
    "ATX-WKS-2207",
    "FAB-LNX-WEB1",
];

/// The Needs Reboot list: the reboot devices' own rollups, so its Pending Patches
/// column and the drill-down opened from it are the same number. The demo used to
/// hardcode those counts, which a drill-down built from the sample rows would then
/// have contradicted on the first click.
fn sample_reboot() -> Vec<DeviceSummary> {
    REBOOT_DEVICE_NAMES
        .iter()
        .filter_map(|name| sample_device_summary(device_id_of(name), false))
        .collect()
}

/// When every sample device last checked in — the snapshot's minute, since the
/// sample fleet is all online.
const SAMPLE_LAST_CONTACT: &str = "2026-06-26 14:31 UTC";

/// One device's facts and per-device rollup over the **whole** sample — the demo
/// counterpart of `rows::apply_device_health`, which reads the unnarrowed current
/// feed: the patch facets (status, severity, search, dates) do not narrow it.
///
/// Pending mirrors the backend's exclude list over the current feed; the sample's
/// `FAILED` rows are install-history records (they carry an installed date), so
/// they count as failed installs rather than as pending. Failed installs are
/// `None` unless the query asked for the Failed status, as on the desktop.
fn sample_device_summary(device_id: i64, failed_queried: bool) -> Option<DeviceSummary> {
    let rows: Vec<PatchRow> = demo_rows()
        .into_iter()
        .map(|d| d.row)
        .filter(|r| r.device_id == device_id)
        .collect();
    let first = rows.first()?;
    let mut counts = SeverityCounts::default();
    let mut aged = 0;
    let mut failed = 0;
    let mut pending = 0;
    for r in &rows {
        match r.status.as_str() {
            "INSTALLED" | "REJECTED" => {}
            "FAILED" => failed += 1,
            _ => {
                pending += 1;
                bump(&mut counts, &r.severity);
                let backlog = severity_rank(&r.severity) >= severity_rank("Important");
                let old = r
                    .first_seen_date
                    .as_deref()
                    .and_then(date_to_epoch)
                    // Per band, like `SlaCutoffs` and the demo's worst devices.
                    .is_none_or(|seen| {
                        seen < SAMPLE_NOW_EPOCH - sla_days_for(&DEMO_SLA, &r.severity) * 86_400
                    });
                if backlog && old {
                    aged += 1;
                }
            }
        }
    }
    Some(DeviceSummary {
        device_id,
        device_name: first.device_name.clone(),
        organization: first.organization.clone(),
        location: first.location.clone(),
        device_role: first.device_role.clone(),
        os_name: first.os_name.clone(),
        pending_count: pending,
        needs_reboot: first.needs_reboot,
        offline: first.offline,
        // Every sample device is a Windows, macOS or Linux agent.
        rollup_scope: RollupScope::Included,
        pending_by_severity: counts,
        aged_critical: aged,
        failed_installs: failed_queried.then_some(failed),
        last_contact: Some(SAMPLE_LAST_CONTACT.to_string()),
    })
}

/// The demo's `device_detail`: the device's rollup over the whole sample, plus its
/// rows as the displayed result filtered them (the Patches tab's rows, as on the
/// desktop). `None` for an id the sample does not have.
pub fn device_detail(
    result: &QueryResult,
    device_id: i64,
    failed_queried: bool,
) -> Option<DeviceDetail> {
    let device = sample_device_summary(device_id, failed_queried)?;
    let rows: Vec<PatchRow> = result
        .rows
        .iter()
        .filter(|r| r.device_id == device_id)
        .cloned()
        .collect();
    Some(DeviceDetail {
        device: Some(device),
        rows_total: rows.len(),
        rows,
    })
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
                if !g.device_names.contains(&r.device_name) {
                    g.affected_devices += 1;
                    g.device_names.push(r.device_name.clone());
                }
                if r.installed_date.as_deref() > g.latest_failure.as_deref() {
                    g.latest_failure = r.installed_date.clone();
                }
            }
            None => groups.push(FailureGroup {
                patch_type: r.patch_type.clone(),
                kb: r.kb.clone(),
                name: r.name.clone(),
                severity: r.severity.clone(),
                affected_devices: 1,
                device_names: vec![r.device_name.clone()],
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
                security: 0,
                moderate: m,
                recommended: 0,
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
/// backend's fixed buckets (`build_age_buckets`), oldest last, with the
/// undated bucket after it.
/// Pending-patch age histogram over the rows on screen, bucketed by how long ago
/// each was first seen. Mirrors the backend's `build_age_buckets`, including its
/// `Unknown` bucket for undated patches — which exists so they cannot silently
/// inflate `181+ days`.
fn scoped_age_buckets(rows: &[PatchRow]) -> Vec<AgeBucket> {
    const LABELS: [&str; 6] = [
        "0-30 days",
        "31-60 days",
        "61-90 days",
        "91-180 days",
        "181+ days",
        "Unknown",
    ];
    let mut counts = [0usize; LABELS.len()];
    for r in rows.iter().filter(|r| r.status == "PENDING") {
        let idx = match r.first_seen_date.as_deref().and_then(date_to_epoch) {
            Some(seen) => {
                let days = (SAMPLE_NOW_EPOCH - seen) / 86_400;
                match days {
                    d if d <= 30 => 0,
                    d if d <= 60 => 1,
                    d if d <= 90 => 2,
                    d if d <= 180 => 3,
                    _ => 4,
                }
            }
            None => 5,
        };
        counts[idx] += 1;
    }
    LABELS
        .iter()
        .zip(counts)
        .map(|(label, count)| AgeBucket {
            label: (*label).to_string(),
            count,
        })
        .collect()
}

/// The demo's SLA policy: a tighter window for Critical so the sample shows a
/// per-band override at work (the Critical patches first seen mid-June are past it
/// by the snapshot date; nothing else is).
const DEMO_SLA: SlaPolicy = SlaPolicy {
    default_days: 30,
    by_severity: SlaBySeverity {
        critical: Some(14),
        important: None,
        security: None,
        moderate: None,
        recommended: None,
        low: None,
        optional: None,
    },
};

/// Mirrors the backend's cap on each device list.
const DEVICE_LIST_LIMIT: usize = 25;

/// Counts one row into its band, keyed by rank — the same classification
/// `severity_rank` gives the sort, so an unmapped label lands in Unknown exactly as
/// the backend's `Severity::from_raw` sends it there.
fn bump(c: &mut SeverityCounts, severity: &str) {
    match severity_rank(severity) {
        7 => c.critical += 1,
        6 => c.important += 1,
        5 => c.security += 1,
        4 => c.moderate += 1,
        3 => c.recommended += 1,
        2 => c.low += 1,
        1 => c.optional += 1,
        _ => c.unknown += 1,
    }
}

/// The worst devices over the pending rows on screen, ranked like the backend's
/// `rows::build_device_backlogs`: past SLA, then the most urgent breakdown, then id.
/// Like the demo's by-OS rollup it derives from the scoped rows (the sample fleet
/// is all online).
fn demo_worst_devices(rows: &[PatchRow]) -> DeviceBacklogList {
    let mut by_device: BTreeMap<i64, DeviceBacklog> = BTreeMap::new();
    for r in rows
        .iter()
        .filter(|r| matches!(r.status.as_str(), "PENDING" | "APPROVED"))
    {
        let d = by_device
            .entry(r.device_id)
            .or_insert_with(|| DeviceBacklog {
                device_id: r.device_id,
                device_name: r.device_name.clone(),
                organization: r.organization.clone(),
                os_name: r.os_name.clone(),
                pending: SeverityCounts::default(),
                pending_total: 0,
                past_sla: 0,
                oldest_first_seen: None,
                latest_collected: None,
            });
        bump(&mut d.pending, &r.severity);
        d.pending_total += 1;
        let seen = r.first_seen_date.as_deref().and_then(date_to_epoch);
        let sla_secs = sla_days_for(&DEMO_SLA, &r.severity) * 86_400;
        if seen.is_none_or(|s| s < SAMPLE_NOW_EPOCH - sla_secs) {
            d.past_sla += 1;
        }
        // YYYY-MM-DD compares chronologically as text.
        if let Some(date) = r.first_seen_date.clone() {
            if d.oldest_first_seen.as_ref().is_none_or(|o| date < *o) {
                d.oldest_first_seen = Some(date.clone());
            }
            if d.latest_collected.as_ref().is_none_or(|l| date > *l) {
                d.latest_collected = Some(date);
            }
        }
    }
    let devices_total = by_device.len();
    let urgency = |c: &SeverityCounts| {
        [
            c.critical,
            c.important,
            c.security,
            c.moderate,
            c.recommended,
            c.low,
            c.optional,
            c.unknown,
        ]
    };
    let mut devices: Vec<DeviceBacklog> = by_device.into_values().collect();
    devices.sort_by(|a, b| {
        b.past_sla
            .cmp(&a.past_sla)
            .then_with(|| urgency(&b.pending).cmp(&urgency(&a.pending)))
            .then_with(|| a.device_id.cmp(&b.device_id))
    });
    devices.truncate(DEVICE_LIST_LIMIT);
    DeviceBacklogList {
        devices,
        devices_total,
    }
}

/// One offline laptop still listed with pending patches, so the demo's offline
/// section has something to show. It sits outside `demo_rows` on purpose: offline
/// devices are excluded from every other rollup, and the sample rows are all online.
fn sample_offline_backlog() -> Vec<DeviceBacklog> {
    vec![DeviceBacklog {
        device_id: device_id_of("ATX-LT-0412"),
        device_name: "ATX-LT-0412".to_string(),
        organization: "Northwind Traders".to_string(),
        os_name: Some("Windows 11 Pro".to_string()),
        pending: SeverityCounts {
            critical: 1,
            important: 2,
            ..SeverityCounts::default()
        },
        pending_total: 3,
        past_sla: 3,
        oldest_first_seen: Some("2026-04-14 00:00 UTC".to_string()),
        latest_collected: Some("2026-05-02 08:10 UTC".to_string()),
    }]
}

/// First seen → installed over the INSTALLED rows on screen, mirroring the
/// backend's `rows::build_time_to_install` (median + nearest-rank p90, overall /
/// by org / by severity, most urgent band first).
fn demo_time_to_install(rows: &[PatchRow], statuses: &[String]) -> TimeToInstall {
    let mut all: Vec<i64> = Vec::new();
    let mut by_org: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    let mut by_sev: BTreeMap<(std::cmp::Reverse<u8>, String), Vec<i64>> = BTreeMap::new();
    let mut installed_records = 0;
    for r in rows.iter().filter(|r| r.status == "INSTALLED") {
        installed_records += 1;
        let seen = r.first_seen_date.as_deref().and_then(date_to_epoch);
        let at = r.installed_date.as_deref().and_then(date_to_epoch);
        let (Some(seen), Some(at)) = (seen, at) else {
            continue;
        };
        if at < seen {
            continue;
        }
        let secs = at - seen;
        all.push(secs);
        by_org.entry(r.organization.clone()).or_default().push(secs);
        by_sev
            .entry((
                std::cmp::Reverse(severity_rank(&r.severity)),
                r.severity.clone(),
            ))
            .or_default()
            .push(secs);
    }
    let latency = |label: String, mut secs: Vec<i64>| {
        secs.sort_unstable();
        InstallLatency {
            label,
            samples: secs.len(),
            median_days: median(&secs) / 86_400.0,
            p90_days: nearest_rank(&secs, 90) as f64 / 86_400.0,
        }
    };
    TimeToInstall {
        installs_queried: statuses.iter().any(|s| s.eq_ignore_ascii_case("INSTALLED")),
        excluded_records: installed_records - all.len(),
        installed_records,
        overall: (!all.is_empty()).then(|| latency("All installs".to_string(), all)),
        by_organization: by_org.into_iter().map(|(o, s)| latency(o, s)).collect(),
        by_severity: by_sev
            .into_iter()
            .map(|((_, label), s)| latency(label, s))
            .collect(),
    }
}

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
    let mut result = assemble(rows, &filter.organization_ids, statuses);
    result.changes = demo_changes(&result.rows, statuses);
    result
}

/// When the sample's "previous run" happened — a day before [`GENERATED_AT`].
const PREVIOUS_RUN_AT: &str = "2026-06-25 14:30:02 UTC";

/// A representative "changes since the previous run" drawn from the filtered rows,
/// so it narrows with every facet the way the real diff does. The backend diffs
/// against a stored snapshot; the demo has no history, so it picks a stable few
/// rows as "new", its failures as "newly failed", and invents two resolved patches
/// on devices in scope (resolved patches are by definition absent from the rows).
fn demo_changes(rows: &[PatchRow], statuses: &[String]) -> RunChanges {
    let selected = |s: &str| statuses.iter().any(|x| x.eq_ignore_ascii_case(s));
    let item = |r: &PatchRow| ChangeItem {
        device_id: r.device_id,
        device_name: r.device_name.clone(),
        patch_type: r.patch_type.clone(),
        kb: r.kb.clone(),
        name: r.name.clone(),
        severity: r.severity.clone(),
    };
    let pending: Vec<&PatchRow> = rows
        .iter()
        .filter(|r| r.status == "PENDING" || r.status == "APPROVED")
        .collect();
    let new_pending_items: Vec<ChangeItem> =
        pending.iter().step_by(5).take(4).map(|r| item(r)).collect();
    let newly_failed_items: Vec<ChangeItem> = rows
        .iter()
        .filter(|r| r.status == "FAILED")
        .take(1)
        .map(item)
        .collect();
    let resolved_items: Vec<ChangeItem> = pending
        .iter()
        .take(2)
        .zip([
            (
                "KB5039212",
                "2024-06 Cumulative Update for Windows Server 2022",
            ),
            ("KB5039894", "2024-06 Servicing Stack Update"),
        ])
        .map(|(r, (kb, name))| ChangeItem {
            kb: Some(kb.to_string()),
            name: name.to_string(),
            severity: "Important".to_string(),
            patch_type: "OS".to_string(),
            ..item(r)
        })
        .collect();
    RunChanges {
        previous_at: Some(PREVIOUS_RUN_AT.to_string()),
        tracks_pending: selected("PENDING") || selected("APPROVED"),
        tracks_failed: selected("FAILED"),
        too_large: false,
        new_pending: new_pending_items.len(),
        resolved: resolved_items.len(),
        newly_failed: newly_failed_items.len(),
        new_pending_items,
        resolved_items,
        newly_failed_items,
    }
}

/// Builds a `QueryResult` from already-filtered display rows, narrowing the rollups
/// to `org_filter` (the organization facet) when one is set. An empty `org_filter`
/// means every organization, matching the real facet.
fn assemble(rows: Vec<PatchRow>, org_filter: &[i64], statuses: &[String]) -> QueryResult {
    // Failures derive from the already-filtered rows, so the tab reacts to filters.
    let failures = demo_failures(&rows);
    let names: Vec<&str> = org_filter.iter().copied().filter_map(org_name).collect();
    let keep = |org: &str| names.is_empty() || names.contains(&org);
    let compliance: Vec<_> = sample_compliance()
        .into_iter()
        .filter(|b| keep(&b.organization))
        .collect();
    let reboot_devices = sample_reboot()
        .into_iter()
        .filter(|d| keep(&d.organization))
        .collect();
    let severity_by_org = sample_severity_by_org()
        .into_iter()
        .filter(|o| keep(&o.organization))
        .collect();
    let offline: Vec<DeviceBacklog> = sample_offline_backlog()
        .into_iter()
        .filter(|d| keep(&d.organization))
        .collect();
    let devices_offline = offline.len();
    // Online devices in the rollups plus the offline ones they exclude — the same
    // reconciliation the backend's scope note states.
    let devices_total = compliance.iter().map(|b| b.devices_total).sum::<usize>() + devices_offline;
    let worst_devices = demo_worst_devices(&rows);
    let time_to_install = demo_time_to_install(&rows, statuses);
    let approvals = sample_approvals(&compliance, &keep);
    // Both of these used to ship whole-fleet regardless of the facet, so with an
    // organization selected the Compliance tab's by-OS chart and the age histogram
    // described a fleet the rest of the screen — and `devices_total` beside them —
    // no longer showed. They are derived from the scoped rows instead, which also
    // means they react to every other facet rather than just this one.
    let compliance_by_os = scoped_compliance_by_os(&rows);
    let age_buckets = scoped_age_buckets(&rows);
    QueryResult {
        rows_total: rows.len(),
        rows,
        reboot_devices,
        compliance,
        compliance_by_os,
        failures,
        severity_by_org,
        age_buckets,
        worst_devices,
        offline_backlog: DeviceBacklogList {
            devices_total: offline.len(),
            devices: offline,
        },
        time_to_install,
        sla_policy: DEMO_SLA,
        approvals,
        devices_total,
        // Both families are represented and one sample laptop is offline, so the
        // demo's scope note reads like a whole-fleet desktop query.
        devices_offline,
        devices_unpatchable: 0,
        patch_families: PatchFamilies {
            os: true,
            software: true,
        },
        // Filled by `filtered_result`, which knows the status selection.
        changes: RunChanges::default(),
        generated_at: GENERATED_AT.to_string(),
        data_fetched_at: GENERATED_AT.to_string(),
    }
}

fn device_matches(d: &DemoRow, f: &FilterParams) -> bool {
    // Empty = every one of them, and within a facet the ids are OR'd — the same
    // semantics `FilterParams::device_allowed` gives the real thing.
    let any = |sel: &[i64], id: i64| sel.is_empty() || sel.contains(&id);
    any(&f.organization_ids, d.org_id)
        && any(&f.location_ids, d.location_id)
        && any(&f.role_ids, d.role_id)
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
        && first_seen_in_window(row, f)
        && install_in_window(row, f, install_after_days)
}

fn type_matches(patch_type: &str, row_type: &str) -> bool {
    match patch_type {
        "OS" | "SOFTWARE" => row_type.eq_ignore_ascii_case(patch_type),
        _ => true, // "ALL" or anything unexpected
    }
}

fn search_matches(row: &PatchRow, query: &str) -> bool {
    // Mirrors `FilterParams::search_allowed`: the `KB` prefix is stripped from BOTH
    // the needle and the KB before comparing, so "KB5040434" finds a patch stored as
    // "5040434" and vice versa. A plain substring match only handled one of those
    // directions, so the demo's search quietly behaved differently from the app's.
    let kb = row.kb.as_deref().unwrap_or("");
    let q = query.trim().to_lowercase();
    let q_bare = q.trim_start_matches("kb").trim().to_string();
    let kb_lower = kb.to_lowercase();
    let kb_bare = kb_lower.trim_start_matches("kb").trim();
    kb_lower.contains(&q) || kb_bare.contains(&q_bare) || row.name.to_lowercase().contains(&q)
}

fn first_seen_in_window(row: &PatchRow, f: &FilterParams) -> bool {
    let Some(released) = row.first_seen_date.as_deref().and_then(date_to_epoch) else {
        // No release date can't satisfy a date window; pass only when none is set.
        return f.detected_within_days.is_none()
            && f.detected_after.is_none()
            && f.detected_before.is_none();
    };
    if let Some(days) = f.detected_within_days
        && released < SAMPLE_NOW_EPOCH - days * 86_400
    {
        return false;
    }
    if let Some(after) = f.detected_after
        && released < after
    {
        return false;
    }
    if let Some(before) = f.detected_before
        && released > before
    {
        return false;
    }
    true
}

fn install_in_window(row: &PatchRow, f: &FilterParams, install_after_days: Option<i64>) -> bool {
    // The window only constrains install-history rows (INSTALLED / FAILED).
    let is_history =
        row.status.eq_ignore_ascii_case("INSTALLED") || row.status.eq_ignore_ascii_case("FAILED");
    if !is_history {
        return true;
    }
    let installed = row.installed_date.as_deref().and_then(date_to_epoch);
    // An absolute range replaces the relative lookback, as it does backend-side.
    if let Some(after) = f.installed_after {
        return installed.is_some_and(|t| t >= after && f.installed_before.is_none_or(|b| t <= b));
    }
    let Some(days) = install_after_days else {
        return true;
    };
    match installed {
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
pub fn group_key(row: &PatchRow, group_by: GroupBy) -> String {
    match group_by {
        GroupBy::Device => row.device_id.to_string(),
        GroupBy::Product if product_of(row).is_some() => {
            format!("SOFTWARE\u{1f}{}", product_of(row).unwrap_or_default())
        }
        GroupBy::Patch | GroupBy::Product => format!(
            "{}\u{1f}{}\u{1f}{}",
            row.patch_type,
            row.kb.as_deref().unwrap_or(""),
            row.name
        ),
    }
}

/// The product a row groups under by product — mirrors `rows::product_of`. The
/// sample spells the type "Software", the backend "SOFTWARE", hence the
/// case-insensitive compare.
fn product_of(row: &PatchRow) -> Option<&str> {
    row.product_identifier
        .as_deref()
        .filter(|p| row.patch_type.eq_ignore_ascii_case("SOFTWARE") && !p.trim().is_empty())
}

/// Groups the sample rows the way `rows::build_groups` groups the real ones:
/// device groups ordered by worst severity then org/device, patch groups by blast
/// radius then severity.
pub fn group_rows(rows: &[PatchRow], group_by: GroupBy) -> Vec<PatchGroup> {
    let mut order: Vec<String> = Vec::new();
    let mut acc: BTreeMap<String, PatchGroup> = BTreeMap::new();
    let mut devices: BTreeMap<String, BTreeSet<i64>> = BTreeMap::new();
    // Product groups only: member titles and their row counts, for the label.
    let mut titles: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for r in rows {
        let key = group_key(r, group_by);
        if group_by == GroupBy::Product && product_of(r).is_some() {
            *titles
                .entry(key.clone())
                .or_default()
                .entry(r.name.clone())
                .or_default() += 1;
        }
        let rank = severity_rank(&r.severity);
        let entry = acc.entry(key.clone()).or_insert_with(|| {
            order.push(key.clone());
            PatchGroup {
                key: key.clone(),
                label: match group_by {
                    GroupBy::Device => r.device_name.clone(),
                    GroupBy::Patch | GroupBy::Product => r.name.clone(),
                },
                sublabel: match group_by {
                    GroupBy::Device => Some(r.organization.clone()),
                    GroupBy::Patch | GroupBy::Product => r.kb.clone().filter(|k| !k.is_empty()),
                },
                rows: 0,
                devices: 0,
                severity: r.severity.clone(),
                severity_rank: rank,
                // Device state, so it belongs to a *device* group only. Every row of
                // a device agrees, hence taking the first. A patch group spans many
                // devices, so "is it offline" has no single answer and the backend
                // deliberately claims neither — a header reading "offline" over a
                // patch installed on forty machines would be meaningless. Copying the
                // first row's state into a patch group is what
                // `grouping_matches_the_backend_byte_for_byte` caught.
                offline: matches!(group_by, GroupBy::Device) && r.offline,
                needs_reboot: matches!(group_by, GroupBy::Device) && r.needs_reboot,
            }
        });
        entry.rows += 1;
        if rank > entry.severity_rank {
            entry.severity_rank = rank;
            entry.severity = r.severity.clone();
        }
        devices.entry(key).or_default().insert(r.device_id);
    }
    let mut out: Vec<PatchGroup> = order
        .into_iter()
        .filter_map(|k| {
            acc.remove(&k).map(|mut g| {
                g.devices = devices.get(&k).map(|d| d.len()).unwrap_or(0);
                // A product group is named for the product and counts its versions,
                // as `rows::build_groups` does.
                if let Some(t) = titles.get(&k) {
                    g.label = product_display_name(t.iter().map(|(title, n)| (title.as_str(), *n)));
                    g.sublabel = (t.len() > 1).then(|| format!("{} versions", t.len()));
                }
                g
            })
        })
        .collect();
    match group_by {
        GroupBy::Device => out.sort_by_key(|g| {
            (
                std::cmp::Reverse(g.severity_rank),
                g.sublabel.clone().unwrap_or_default().to_lowercase(),
                g.label.to_lowercase(),
            )
        }),
        GroupBy::Patch | GroupBy::Product => out.sort_by_key(|g| {
            (
                std::cmp::Reverse(g.devices),
                std::cmp::Reverse(g.severity_rank),
            )
        }),
    }
    out
}

/// The sample rows belonging to one group.
pub fn group_members(rows: &[PatchRow], group_by: GroupBy, key: &str) -> Vec<PatchRow> {
    rows.iter()
        .filter(|r| group_key(r, group_by) == key)
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter() -> FilterParams {
        FilterParams::default()
    }

    /// The reboot list and the names the group headers flag must be the same set, or
    /// the Needs Reboot tab and the Patches tab disagree about which machines they
    /// are. The backend derives both from one device inventory; the demo has none, so
    /// this const is the single source and this test is what holds it there.
    #[test]
    fn the_reboot_name_list_matches_the_reboot_tab() {
        let from_tab: Vec<String> = sample_reboot().into_iter().map(|d| d.device_name).collect();
        assert_eq!(
            from_tab,
            REBOOT_DEVICE_NAMES.to_vec(),
            "REBOOT_DEVICE_NAMES must list exactly the devices sample_reboot() returns"
        );
    }

    /// The demo's group headers used to hardcode `offline: false` / `needs_reboot:
    /// false`, so those badges were unreachable in the demo no matter what the rows
    /// said. The backend copies both from the group's first row.
    #[test]
    fn group_headers_carry_the_reboot_flag_from_the_sample() {
        let rows = demo_rows()
            .into_iter()
            .map(|d| d.row)
            .collect::<Vec<PatchRow>>();
        let groups = group_rows(&rows, GroupBy::Device);
        assert!(
            groups.iter().any(|g| g.needs_reboot),
            "at least one sample device needs a reboot, so some group header must say so"
        );
    }

    /// Mirrors `FilterParams::search_allowed`, which strips the `KB` prefix from both
    /// sides. A plain substring match only covered one direction.
    #[test]
    fn demo_search_strips_the_kb_prefix_on_either_side() {
        let rows: Vec<PatchRow> = demo_rows().into_iter().map(|d| d.row).collect();
        let with_kb = rows
            .iter()
            .find(|r| r.kb.as_deref().is_some_and(|k| !k.is_empty()))
            .expect("the sample has at least one KB-bearing row");
        let kb = with_kb.kb.clone().unwrap();
        let bare = kb.trim_start_matches("KB").to_string();

        assert!(search_matches(with_kb, &kb), "exact KB must match");
        assert!(
            search_matches(with_kb, &bare),
            "the bare number must match a KB-prefixed patch"
        );
        assert!(
            search_matches(with_kb, &format!("kb{bare}")),
            "a lowercase kb-prefixed needle must match too"
        );
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
        assert_eq!(
            r.devices_total,
            summed + r.devices_offline + r.devices_unpatchable,
            "the scope note's reconciliation: in-rollup + offline + non-patchable"
        );
        assert!(
            r.compliance
                .iter()
                .all(|b| b.devices_compliant <= b.devices_total)
        );
    }

    #[test]
    fn date_to_epoch_matches_known_dates() {
        assert_eq!(date_to_epoch("2026-01-01"), Some(1_767_225_600));
        assert_eq!(date_to_epoch("2026-06-26"), Some(1_782_432_000));
        assert_eq!(date_to_epoch("nonsense"), None);
        assert_eq!(date_to_epoch("2026-13-01"), None);
    }

    #[test]
    fn the_demo_reports_changes_that_narrow_with_the_filters() {
        let all = filtered_result(&filter(), "ALL", &all_statuses(), Some(3650));
        let c = &all.changes;
        assert!(c.previous_at.is_some());
        assert!(c.new_pending > 0 && c.resolved > 0 && c.newly_failed > 0);
        assert_eq!(c.new_pending, c.new_pending_items.len());
        assert!(c.tracks_pending && c.tracks_failed);

        let failed_only = filtered_result(&filter(), "ALL", &["FAILED".to_string()], Some(3650));
        assert!(!failed_only.changes.tracks_pending);
        assert_eq!(failed_only.changes.new_pending, 0);
        let devices: BTreeSet<&str> = all.rows.iter().map(|r| r.device_name.as_str()).collect();
        assert!(
            c.resolved_items
                .iter()
                .all(|i| devices.contains(i.device_name.as_str())),
            "resolved patches sit on devices in scope"
        );
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
            organization_ids: vec![1], // Contoso Ltd
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
    fn the_demo_carries_every_backlog_and_install_time_aggregate() {
        let r = filtered_result(&filter(), "ALL", &all_statuses(), Some(3650));
        assert!(!r.worst_devices.devices.is_empty());
        assert_eq!(r.worst_devices.devices_total, r.worst_devices.devices.len());
        // The Critical override (14 days) is what puts a device past SLA: every
        // sample patch is younger than the 30-day default.
        let first = &r.worst_devices.devices[0];
        assert!(
            first.past_sla > 0 && first.pending.critical > 0,
            "{first:?}"
        );
        assert!(
            r.worst_devices
                .devices
                .windows(2)
                .all(|w| w[0].past_sla >= w[1].past_sla),
            "ranked by past SLA first"
        );
        assert_eq!(r.offline_backlog.devices.len(), r.devices_offline);
        assert_eq!(r.sla_policy.by_severity.critical, Some(14));

        let t = &r.time_to_install;
        assert!(t.installs_queried);
        let overall = t.overall.as_ref().expect("the sample has installs");
        assert_eq!(overall.samples, t.installed_records - t.excluded_records);
        assert_eq!(t.by_severity[0].label, "Critical", "most urgent band first");

        // Without the Installed status the table says why it is empty.
        let pending = filtered_result(&filter(), "ALL", &["PENDING".to_string()], None);
        assert!(!pending.time_to_install.installs_queried);
        assert!(pending.time_to_install.overall.is_none());
    }

    #[test]
    fn the_offline_sample_narrows_with_the_org_facet() {
        let contoso = FilterParams {
            organization_ids: vec![1],
            ..filter()
        };
        let r = filtered_result(&contoso, "ALL", &all_statuses(), Some(3650));
        assert!(r.offline_backlog.devices.is_empty());
        assert_eq!(r.devices_offline, 0);
    }

    #[test]
    fn dashboard_rollups_are_always_populated() {
        let r = filtered_result(&filter(), "ALL", &all_statuses(), Some(3650));
        assert!(!r.severity_by_org.is_empty());
        assert_eq!(
            r.age_buckets.len(),
            6,
            "fixed six-bucket histogram (five ages + unknown)"
        );
    }

    /// The module's contract is that the rollups are "narrowed only by the
    /// organization facet" — but `compliance_by_os` and `age_buckets` were shipped
    /// whole-fleet regardless, so with an org selected the Compliance tab's by-OS
    /// chart and the age histogram described a fleet that `devices_total` beside
    /// them no longer showed.
    #[test]
    fn the_by_os_and_age_rollups_narrow_with_the_org_facet() {
        let all = filtered_result(&filter(), "ALL", &all_statuses(), Some(3650));
        let scoped = filtered_result(
            &FilterParams {
                organization_ids: vec![1],
                ..filter()
            },
            "ALL",
            &all_statuses(),
            Some(3650),
        );

        let all_os_devices: usize = all.compliance_by_os.iter().map(|o| o.devices_total).sum();
        let scoped_os_devices: usize = scoped
            .compliance_by_os
            .iter()
            .map(|o| o.devices_total)
            .sum();
        assert!(
            scoped_os_devices < all_os_devices,
            "the by-OS chart must shrink with the org facet ({scoped_os_devices} vs {all_os_devices})"
        );

        let all_aged: usize = all.age_buckets.iter().map(|b| b.count).sum();
        let scoped_aged: usize = scoped.age_buckets.iter().map(|b| b.count).sum();
        assert!(
            scoped_aged < all_aged,
            "the age histogram must shrink with the org facet ({scoped_aged} vs {all_aged})"
        );
        assert_eq!(scoped.age_buckets.len(), 6, "the bucket set stays fixed");
    }

    /// The age histogram counts pending patches only, so a query that asks for
    /// install history must not populate it.
    #[test]
    fn the_age_histogram_counts_only_pending_patches() {
        let installed = filtered_result(&filter(), "ALL", &["INSTALLED".to_string()], Some(3650));
        assert_eq!(
            installed.age_buckets.iter().map(|b| b.count).sum::<usize>(),
            0,
            "installed patches are not pending backlog"
        );
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

    /// The backend's approval totals are the compliance columns summed (one
    /// population), and the organization facet narrows the stuck list too.
    #[test]
    fn approval_totals_match_the_compliance_columns_and_follow_the_org_facet() {
        let all = filtered_result(&filter(), "ALL", &all_statuses(), None);
        let sum = |r: &QueryResult, f: fn(&ComplianceBucket) -> usize| {
            r.compliance.iter().map(f).sum::<usize>()
        };
        assert_eq!(
            all.approvals.awaiting_approval,
            sum(&all, |b| b.awaiting_approval)
        );
        assert_eq!(
            all.approvals.approved_not_installed,
            sum(&all, |b| b.approved_not_installed)
        );
        assert!(
            all.approvals.stuck_devices_total > 0,
            "the demo shows the card populated"
        );

        let northwind = FilterParams {
            organization_ids: vec![2],
            ..filter()
        };
        let r = filtered_result(&northwind, "ALL", &all_statuses(), None);
        assert!(
            r.approvals
                .stuck_devices
                .iter()
                .all(|d| d.organization == "Northwind Traders")
        );
        assert_eq!(
            r.approvals.awaiting_approval,
            sum(&r, |b| b.awaiting_approval)
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
        // Contoso (id 1) has two locations; an unknown org has none. With no org
        // selected the picker offers every location, matching `list_locations`.
        assert_eq!(sample_locations(&[1]).len(), 2);
        assert!(sample_locations(&[999]).is_empty());
        assert_eq!(sample_locations(&[]).len(), LOCATIONS.len());
        assert!(sample_locations(&[1, 2]).len() > sample_locations(&[1]).len());
    }

    /// The grouping this module re-implements, asserted against the backend's own
    /// output rather than against a hand-written expectation.
    ///
    /// The property tests below say device groups lead with the worst severity and
    /// patch groups partition the rows — both of which a wrong implementation can
    /// satisfy. Commit cc33b0a is the proof: demo group headers hardcoded
    /// `offline: false` / `needs_reboot: false`, and demo search matched substrings
    /// where the backend strips a `KB` prefix. Every property test stayed green.
    ///
    /// The fixture is emitted by `rows::tests::demo_grouping_fixture_is_current`, so
    /// neither side can move without the other going red, and the expectation is the
    /// backend itself rather than someone's memory of it.
    #[test]
    fn grouping_matches_the_backend_byte_for_byte() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Fixture {
            rows: Vec<PatchRow>,
            by_device: Vec<PatchGroup>,
            by_patch: Vec<PatchGroup>,
            by_product: Vec<PatchGroup>,
            keys_by_device: Vec<String>,
            keys_by_patch: Vec<String>,
            keys_by_product: Vec<String>,
        }
        let fixture: Fixture = serde_json::from_str(include_str!("../tests/backend-grouping.json"))
            .expect("the committed backend fixture parses");

        for (group_by, expected, expected_keys) in [
            (GroupBy::Device, &fixture.by_device, &fixture.keys_by_device),
            (GroupBy::Patch, &fixture.by_patch, &fixture.keys_by_patch),
            (
                GroupBy::Product,
                &fixture.by_product,
                &fixture.keys_by_product,
            ),
        ] {
            let actual = group_rows(&fixture.rows, group_by);
            assert_eq!(
                actual.len(),
                expected.len(),
                "{group_by:?}: group count differs from the backend"
            );
            for (got, want) in actual.iter().zip(expected) {
                // Compared field by field so a failure names the field, which is how
                // the hardcoded offline/needs_reboot bug would have read.
                assert_eq!(got.key, want.key, "{group_by:?} key");
                assert_eq!(got.label, want.label, "{group_by:?} label");
                assert_eq!(got.sublabel, want.sublabel, "{group_by:?} sublabel");
                assert_eq!(got.rows, want.rows, "{group_by:?} rows");
                assert_eq!(got.devices, want.devices, "{group_by:?} devices");
                assert_eq!(got.severity, want.severity, "{group_by:?} severity");
                assert_eq!(
                    got.severity_rank, want.severity_rank,
                    "{group_by:?} severity_rank"
                );
                assert_eq!(got.offline, want.offline, "{group_by:?} offline");
                assert_eq!(
                    got.needs_reboot, want.needs_reboot,
                    "{group_by:?} needs_reboot"
                );
            }
            let keys: Vec<String> = fixture
                .rows
                .iter()
                .map(|r| group_key(r, group_by))
                .collect();
            assert_eq!(
                &keys, expected_keys,
                "{group_by:?}: group_key must produce the backend's opaque keys — the \
                 frontend echoes them back to get_patch_group_members"
            );
        }
    }

    #[test]
    fn group_rows_by_device_and_by_patch_mirror_the_backend_ordering() {
        let rows = filtered_result(&FilterParams::default(), "ALL", &["PENDING".into()], None).rows;
        assert!(!rows.is_empty(), "the sample must produce rows to group");

        // Device groups: one per device, worst severity first.
        let by_device = group_rows(&rows, GroupBy::Device);
        let distinct: BTreeSet<i64> = rows.iter().map(|r| r.device_id).collect();
        assert_eq!(by_device.len(), distinct.len());
        assert!(by_device.iter().all(|g| g.devices == 1));
        assert!(
            by_device
                .windows(2)
                .all(|w| w[0].severity_rank >= w[1].severity_rank),
            "device groups lead with the worst severity"
        );

        // Patch groups: blast radius first, and every row is accounted for.
        let by_patch = group_rows(&rows, GroupBy::Patch);
        assert_eq!(by_patch.iter().map(|g| g.rows).sum::<usize>(), rows.len());
        assert!(
            by_patch.windows(2).all(|w| w[0].devices >= w[1].devices),
            "patch groups lead with blast radius"
        );
    }

    /// The sample carries two versions of one Chrome build pair, so the By product
    /// view has a multi-version group to show — named for the product.
    #[test]
    fn the_sample_folds_chrome_versions_into_one_product_group() {
        let rows = filtered_result(&FilterParams::default(), "ALL", &all_statuses(), None).rows;
        let groups = group_rows(&rows, GroupBy::Product);
        let chrome = groups
            .iter()
            .find(|g| g.label == "Google Chrome")
            .expect("a Google Chrome product group");
        assert_eq!(chrome.sublabel.as_deref(), Some("2 versions"));
        assert!(
            groups.len() < group_rows(&rows, GroupBy::Patch).len(),
            "folding versions yields fewer groups than one per title"
        );
    }

    #[test]
    fn group_members_partition_the_rows_exactly() {
        let rows = filtered_result(&FilterParams::default(), "ALL", &["PENDING".into()], None).rows;
        for group_by in [GroupBy::Device, GroupBy::Patch, GroupBy::Product] {
            let groups = group_rows(&rows, group_by);
            let mut seen = 0usize;
            for g in &groups {
                let members = group_members(&rows, group_by, &g.key);
                assert_eq!(members.len(), g.rows, "header count must match its members");
                seen += members.len();
            }
            assert_eq!(seen, rows.len(), "every row belongs to exactly one group");
        }
        assert!(group_members(&rows, GroupBy::Device, "no-such-key").is_empty());
    }

    fn statuses(s: &[&str]) -> Vec<String> {
        s.iter().map(|s| s.to_string()).collect()
    }

    /// The Needs Reboot tab and the drill-down opened from it must show the same
    /// pending count — the demo once hardcoded the tab's numbers, which the
    /// drill-down (built from the sample rows) would have contradicted.
    #[test]
    fn the_reboot_tab_and_the_drill_down_agree() {
        let r = filtered_result(&filter(), "ALL", &statuses(&["PENDING"]), None);
        for d in &r.reboot_devices {
            let detail = device_detail(&r, d.device_id, false).expect("a sample device");
            let device = detail.device.expect("with facts");
            assert_eq!(device.pending_count, d.pending_count, "{}", d.device_name);
            assert!(
                device.needs_reboot,
                "{} is on the reboot tab",
                d.device_name
            );
        }
    }

    /// The per-device rollup is fleet-tier, like the backend's: the patch facets
    /// narrow the rows under it, never the counts. Failed installs are unknown
    /// unless the query asked for Failed.
    #[test]
    fn the_drill_down_counts_ignore_patch_filters_and_its_rows_do_not() {
        let critical_only = FilterParams {
            severities: vec!["CRITICAL".into()],
            ..filter()
        };
        let r = filtered_result(
            &critical_only,
            "ALL",
            &statuses(&["PENDING", "APPROVED"]),
            None,
        );
        let id = device_id_of("DCA-APP01");
        let detail = device_detail(&r, id, false).expect("a sample device");
        let device = detail.device.expect("with facts");
        assert_eq!(device.pending_by_severity.critical, 1);
        assert_eq!(
            device.pending_by_severity.important, 1,
            "the Important patch still counts"
        );
        assert_eq!(device.pending_count, 2);
        assert_eq!(device.failed_installs, None, "Failed was not queried");
        assert_eq!(
            detail.rows_total, 1,
            "only the Critical row passed the filter"
        );
        assert!(detail.rows.iter().all(|row| row.device_id == id));

        let with_failed = device_detail(&r, device_id_of("DCA-WEB02"), true)
            .and_then(|d| d.device)
            .expect("a sample device");
        assert_eq!(with_failed.failed_installs, Some(1));
        assert_eq!(
            with_failed.pending_count, 0,
            "a FAILED install is not pending"
        );

        assert!(
            device_detail(&r, 42, false).is_none(),
            "not a sample device"
        );
    }
}
