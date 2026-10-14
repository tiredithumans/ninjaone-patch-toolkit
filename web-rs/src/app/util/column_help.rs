//! What every compliance-related table column means, as (header, help) pairs the
//! tables render their headers from — so a header cannot be drawn without its
//! definition (`every_header_is_defined_or_an_identity_column`), and the headers
//! cannot drift from the backend's `rows::TableColumn` spellings
//! (`headers_match_the_backend_columns` reads them out of the IPC fixture).
//!
//! Per table rather than one label → help map: the same header can mean different
//! things in two tables ("Approved, Not Installed" counts every approved patch in
//! the compliance table, only the stuck ones in the stuck-approvals table).

/// A column header and its definition; the definition is empty only for an
/// [`IDENTITY_COLUMNS`] header, which needs none.
pub(crate) type HelpedColumn = (&'static str, &'static str);

/// Headers that name the row rather than measure it.
#[cfg(test)]
pub(crate) const IDENTITY_COLUMNS: [&str; 6] = [
    "Organization",
    "OS",
    "Device",
    "Location",
    "Device Role",
    "Group",
];

const DEVICES: &str = "Online Windows, macOS and Linux devices in scope. Offline and non-patchable devices are not counted.";
const WITHIN_SLA: &str = "Devices with no pending patch past its severity's SLA window.";
const WITHIN_SLA_PCT: &str =
    "Within SLA as a share of Devices, to one decimal; never rounded up to 100%.";
const FULLY_PATCHED: &str = "Devices with no pending patch of any severity or age.";
const FULLY_PATCHED_PCT: &str =
    "Fully Patched as a share of Devices, to one decimal; never rounded up to 100%.";
const URGENT_PENDING: &str = "Pending Critical, Important and Security patches.";
const URGENT_PAST_SLA: &str =
    "Urgent Pending patches first seen longer ago than their severity's SLA window, or undated.";
const AWAITING_APPROVAL: &str = "Pending patches, any severity, waiting for an approval decision in NinjaOne — waiting on a person.";
const APPROVED_NOT_INSTALLED: &str =
    "Patches, any severity, approved in NinjaOne but not yet installed — waiting on the agent.";
const PAST_SLA: &str = "Pending patches first seen longer ago than their severity's SLA window, or undated. Severities with No SLA never count.";
const PENDING_PATCHES: &str = "Pending patches of every severity.";
const OLDEST_FIRST_SEEN: &str = "When this device's oldest pending patch was first seen.";
const PENDING_BY_SEVERITY: &str = "This device's pending patches, by severity.";
const CHANGE: &str = "Against the previous comparable run.";

/// The per-organization compliance table (`rows::ComplianceBucket::COLUMNS`).
pub(crate) const COMPLIANCE_ORG_HEADERS: [HelpedColumn; 10] = [
    ("Organization", ""),
    ("Devices", DEVICES),
    ("Within SLA", WITHIN_SLA),
    ("Within SLA %", WITHIN_SLA_PCT),
    ("Fully Patched", FULLY_PATCHED),
    ("Fully Patched %", FULLY_PATCHED_PCT),
    ("Urgent Pending", URGENT_PENDING),
    ("Urgent Past SLA", URGENT_PAST_SLA),
    ("Awaiting Approval", AWAITING_APPROVAL),
    ("Approved, Not Installed", APPROVED_NOT_INSTALLED),
];

/// The per-OS compliance table (`rows::OsCompliance::COLUMNS`).
pub(crate) const COMPLIANCE_OS_HEADERS: [HelpedColumn; 10] = [
    ("OS", ""),
    ("Devices", DEVICES),
    ("Within SLA", WITHIN_SLA),
    ("Within SLA %", WITHIN_SLA_PCT),
    ("Fully Patched", FULLY_PATCHED),
    ("Fully Patched %", FULLY_PATCHED_PCT),
    ("Urgent Pending", URGENT_PENDING),
    ("Urgent Past SLA", URGENT_PAST_SLA),
    ("Awaiting Approval", AWAITING_APPROVAL),
    ("Approved, Not Installed", APPROVED_NOT_INSTALLED),
];

/// The worst-devices list (`rows::DeviceBacklog::WORST_COLUMNS`).
pub(crate) const WORST_HEADERS: [HelpedColumn; 7] = [
    ("Organization", ""),
    ("Device", ""),
    ("OS", ""),
    ("Past SLA", PAST_SLA),
    ("Pending Patches", PENDING_PATCHES),
    ("Oldest First Seen", OLDEST_FIRST_SEEN),
    ("Pending by Severity", PENDING_BY_SEVERITY),
];

/// The offline-backlog list (`rows::DeviceBacklog::OFFLINE_COLUMNS`).
pub(crate) const OFFLINE_HEADERS: [HelpedColumn; 8] = [
    ("Organization", ""),
    ("Device", ""),
    ("OS", ""),
    (
        "Latest Patch Data Collected",
        "The newest collection time on this device's pending records: how current this list is for it.",
    ),
    (
        "Last Contact",
        "When this device's agent last checked in with NinjaOne.",
    ),
    ("Pending Patches", PENDING_PATCHES),
    ("Past SLA", PAST_SLA),
    ("Pending by Severity", PENDING_BY_SEVERITY),
];

/// The stuck-approvals table (`rows::StuckDevice::COLUMNS`).
pub(crate) const STUCK_HEADERS: [HelpedColumn; 4] = [
    ("Device", ""),
    ("Organization", ""),
    (
        "Approved, Not Installed",
        "Approved patches on this device still not installed more than the default SLA window after first seen.",
    ),
    (
        "Oldest First Seen",
        "When the oldest of those patches was first seen.",
    ),
];

/// The first seen → installed table (`rows::InstallLatency::COLUMNS`, less the
/// "(Days)" its cells carry themselves).
pub(crate) const TIME_TO_INSTALL_HEADERS: [HelpedColumn; 5] = [
    ("Breakdown", "Overall, by organization, or by severity."),
    ("Group", ""),
    (
        "Installs Measured",
        "Installed records with both a first-seen and an install time.",
    ),
    (
        "First Seen \u{2192} Installed (Median)",
        "Half of the measured installs landed this soon after first seen, or sooner.",
    ),
    (
        "90th Percentile",
        "Nine in ten measured installs landed this soon after first seen, or sooner.",
    ),
];

/// The Trend tab's per-organization movement table.
pub(crate) const TREND_ORG_HEADERS: [HelpedColumn; 9] = [
    ("Organization", ""),
    ("Within SLA %", WITHIN_SLA_PCT),
    ("Change", CHANGE),
    ("Pending Patches", PENDING_PATCHES),
    ("Change", CHANGE),
    ("Urgent Pending", URGENT_PENDING),
    ("Change", CHANGE),
    ("Urgent Past SLA", URGENT_PAST_SLA),
    ("Change", CHANGE),
];

/// The device drill-down's health labels.
pub(crate) const DEVICE_HEALTH_LABELS: [HelpedColumn; 4] = [
    ("Pending Patches", PENDING_PATCHES),
    (
        "Within SLA",
        "Yes when none of this device's pending patches is past its severity's SLA window.",
    ),
    ("Urgent Past SLA", URGENT_PAST_SLA),
    (
        "Failed Installs",
        "FAILED install records in the query's install window; unknown unless the query included the Failed status.",
    ),
];
