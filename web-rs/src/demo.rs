//! Sample-data builder for the app's demo mode.
//!
//! Produces a fully-formed [`QueryResult`] (and a static OS-type list) from invented
//! orgs/devices/patches so the UI can render populated tables with no NinjaOne
//! account, no sign-in, and no real fleet data. Two callers use it:
//! - the "Load sample data" button (`AppState::load_demo`), for demos/screenshots; and
//! - browser/web mode (the GitHub Pages demo), where there is no Tauri backend at all.
//!
//! It is pure data — no `js_sys`, no IPC — so it compiles and unit-tests on the host
//! target via `just web-test`, like the helpers in [`crate::app::util`].

use crate::types::{ComplianceBucket, DeviceSummary, NodeClass, PatchRow, QueryResult};

/// Wall-clock label shown in the results summary. Fixed (not "now") so the build
/// stays deterministic and host-testable — it reads as a representative snapshot.
const GENERATED_AT: &str = "2026-06-26 14:32:08 UTC";

fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// One sample patch row. Arg order mirrors the Patches table columns left-to-right.
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
) -> PatchRow {
    PatchRow {
        device_name: device.to_string(),
        organization: org.to_string(),
        location: opt(location),
        device_role: opt(role),
        os_name: opt(os),
        patch_type: patch_type.to_string(),
        kb: opt(kb),
        name: name.to_string(),
        // Severity renders in title case (see `app::util::sev_class`); status is
        // upper-case (`status_class`). Mismatched casing just drops the color pill.
        severity: severity.to_string(),
        status: status.to_string(),
        release_date: opt(release_date),
        installed_date: opt(installed_date),
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

/// A representative `QueryResult`: a single page of patch rows plus the compliance
/// and needs-reboot rollups, across three invented organizations.
pub fn sample_query_result() -> QueryResult {
    // (org, location, role, device, os, type, kb, name, severity, status, released, installed)
    let rows = vec![
        // --- Contoso Ltd ---
        row(
            "Contoso Ltd",
            "HQ — Seattle",
            "Domain Controller",
            "SEA-DC01",
            "Windows Server 2022",
            "OS",
            "KB5062553",
            "2026-06 Cumulative Update for Windows Server 2022 (KB5062553)",
            "Critical",
            "PENDING",
            "2026-06-09",
            "",
        ),
        row(
            "Contoso Ltd",
            "HQ — Seattle",
            "Web Server",
            "SEA-WEB01",
            "Windows Server 2022",
            "OS",
            "KB5062553",
            "2026-06 Cumulative Update for Windows Server 2022 (KB5062553)",
            "Critical",
            "PENDING",
            "2026-06-09",
            "",
        ),
        row(
            "Contoso Ltd",
            "Datacenter A",
            "Application Server",
            "DCA-APP01",
            "Windows Server 2019",
            "OS",
            "KB5062561",
            "2026-06 Cumulative Update for Windows Server 2019 (KB5062561)",
            "Important",
            "APPROVED",
            "2026-06-09",
            "",
        ),
        row(
            "Contoso Ltd",
            "Datacenter A",
            "Application Server",
            "DCA-APP01",
            "Windows Server 2019",
            "Software",
            "",
            "Adobe Acrobat Reader 26.001.20512",
            "Critical",
            "PENDING",
            "2026-06-12",
            "",
        ),
        row(
            "Contoso Ltd",
            "HQ — Seattle",
            "Workstation",
            "SEA-WKS-1042",
            "Windows 11 Pro",
            "OS",
            "KB5062554",
            "2026-06 Cumulative Update for Windows 11 24H2 (KB5062554)",
            "Critical",
            "INSTALLED",
            "2026-06-10",
            "2026-06-12",
        ),
        row(
            "Contoso Ltd",
            "HQ — Seattle",
            "Workstation",
            "SEA-WKS-1042",
            "Windows 11 Pro",
            "Software",
            "",
            "Google Chrome 137.0.7151.69",
            "Important",
            "INSTALLED",
            "2026-06-11",
            "2026-06-12",
        ),
        row(
            "Contoso Ltd",
            "HQ — Seattle",
            "Workstation",
            "SEA-WKS-1077",
            "Windows 11 Pro",
            "Software",
            "",
            "Microsoft Edge 137.0.3296.62",
            "Low",
            "PENDING",
            "2026-06-11",
            "",
        ),
        row(
            "Contoso Ltd",
            "Datacenter A",
            "Web Server",
            "DCA-WEB02",
            "Windows Server 2022",
            "OS",
            "KB5062553",
            "2026-06 Cumulative Update for Windows Server 2022 (KB5062553)",
            "Critical",
            "FAILED",
            "2026-06-09",
            "2026-06-13",
        ),
        // --- Northwind Traders ---
        row(
            "Northwind Traders",
            "Datacenter B",
            "Database Server",
            "NW-SQL01",
            "Windows Server 2022",
            "OS",
            "KB5062553",
            "2026-06 Cumulative Update for Windows Server 2022 (KB5062553)",
            "Critical",
            "PENDING",
            "2026-06-09",
            "",
        ),
        row(
            "Northwind Traders",
            "Datacenter B",
            "Database Server",
            "NW-SQL01",
            "Windows Server 2022",
            "Software",
            "",
            "7-Zip 24.09",
            "Moderate",
            "PENDING",
            "2026-06-05",
            "",
        ),
        row(
            "Northwind Traders",
            "Datacenter B",
            "File Server",
            "NW-FILE01",
            "Windows Server 2019",
            "OS",
            "KB5062561",
            "2026-06 Cumulative Update for Windows Server 2019 (KB5062561)",
            "Important",
            "INSTALLED",
            "2026-06-09",
            "2026-06-11",
        ),
        row(
            "Northwind Traders",
            "Branch — Austin",
            "Workstation",
            "ATX-WKS-2207",
            "Windows 10 Pro",
            "OS",
            "KB5062560",
            "2026-06 Cumulative Update for Windows 10 22H2 (KB5062560)",
            "Important",
            "PENDING",
            "2026-06-10",
            "",
        ),
        row(
            "Northwind Traders",
            "Branch — Austin",
            "Workstation",
            "ATX-WKS-2207",
            "Windows 10 Pro",
            "Software",
            "",
            "Mozilla Firefox 140.0",
            "Moderate",
            "REJECTED",
            "2026-06-10",
            "",
        ),
        row(
            "Northwind Traders",
            "Branch — Austin",
            "Workstation",
            "ATX-MAC-0099",
            "macOS 15.5 Sequoia",
            "OS",
            "",
            "macOS 15.5 Security Update 2026-003",
            "Important",
            "PENDING",
            "2026-06-09",
            "",
        ),
        row(
            "Northwind Traders",
            "Branch — Austin",
            "Workstation",
            "ATX-MAC-0099",
            "macOS 15.5 Sequoia",
            "Software",
            "",
            "Google Chrome 137.0.7151.69",
            "Important",
            "INSTALLED",
            "2026-06-11",
            "2026-06-12",
        ),
        row(
            "Northwind Traders",
            "Datacenter B",
            "Application Server",
            "NW-APP05",
            "Windows Server 2022",
            "Software",
            "",
            "Notepad++ 8.7.6",
            "Low",
            "APPROVED",
            "2026-06-03",
            "",
        ),
        // --- Fabrikam Inc ---
        row(
            "Fabrikam Inc",
            "Cloud — us-east-1",
            "Application Server",
            "FAB-LNX-APP3",
            "Ubuntu 22.04 LTS",
            "Software",
            "",
            "OpenSSL 3.0.16 (libssl)",
            "Critical",
            "PENDING",
            "2026-06-08",
            "",
        ),
        row(
            "Fabrikam Inc",
            "Cloud — us-east-1",
            "Application Server",
            "FAB-LNX-APP3",
            "Ubuntu 22.04 LTS",
            "Software",
            "",
            "Docker Engine 28.1.1",
            "Important",
            "INSTALLED",
            "2026-06-06",
            "2026-06-10",
        ),
        row(
            "Fabrikam Inc",
            "Cloud — us-east-1",
            "Web Server",
            "FAB-LNX-WEB1",
            "Ubuntu 22.04 LTS",
            "Software",
            "",
            "nginx 1.27.5",
            "Important",
            "PENDING",
            "2026-06-07",
            "",
        ),
        row(
            "Fabrikam Inc",
            "Cloud — us-east-1",
            "Web Server",
            "FAB-LNX-WEB1",
            "Ubuntu 22.04 LTS",
            "Software",
            "",
            "OpenSSL 3.0.16 (libssl)",
            "Critical",
            "FAILED",
            "2026-06-08",
            "2026-06-11",
        ),
        row(
            "Fabrikam Inc",
            "Cloud — us-east-1",
            "Workstation",
            "FAB-WKS-3310",
            "Windows 11 Pro",
            "OS",
            "KB5062554",
            "2026-06 Cumulative Update for Windows 11 24H2 (KB5062554)",
            "Critical",
            "PENDING",
            "2026-06-10",
            "",
        ),
        row(
            "Fabrikam Inc",
            "Cloud — us-east-1",
            "Workstation",
            "FAB-WKS-3310",
            "Windows 11 Pro",
            "Software",
            "",
            "Microsoft Edge 137.0.3296.62",
            "Low",
            "INSTALLED",
            "2026-06-11",
            "2026-06-12",
        ),
    ];
    let rows_total = rows.len();

    let compliance = vec![
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
    ];

    let reboot_devices = vec![
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
    ];

    QueryResult {
        rows,
        rows_total,
        reboot_devices,
        compliance,
        // Sum of the per-org buckets above (18 + 14 + 10).
        devices_total: 42,
        generated_at: GENERATED_AT.to_string(),
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_result_is_internally_consistent() {
        let r = sample_query_result();
        // Page-0 rows are the whole sample (no second page to fetch over IPC).
        assert!(!r.rows.is_empty());
        assert_eq!(r.rows_total, r.rows.len());
        // Rollups are populated so every tab renders, not just Patches.
        assert!(!r.compliance.is_empty());
        assert!(!r.reboot_devices.is_empty());
        // devices_total matches the sum of the per-org compliance buckets.
        let summed: usize = r.compliance.iter().map(|b| b.devices_total).sum();
        assert_eq!(r.devices_total, summed);
        // Compliant never exceeds total in any bucket.
        assert!(
            r.compliance
                .iter()
                .all(|b| b.devices_compliant <= b.devices_total)
        );
    }

    #[test]
    fn sample_node_classes_match_the_backend_facet() {
        let classes = sample_node_classes();
        assert_eq!(classes.len(), 6);
        assert_eq!(classes[0].value, "WINDOWS_SERVER");
        assert_eq!(classes[0].label, "Windows Server");
    }
}
