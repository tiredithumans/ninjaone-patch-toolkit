//! Fleet-health rollups over the scoped inventory: per-device summaries (the
//! Needs-Reboot view), compliance by organization and by OS, pending counts,
//! the `rollup_device` population every rollup shares, and the scope note that
//! states what that population excludes.

use std::borrow::Cow;
use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::model::{Device, Patch, Severity};

use super::join::UNKNOWN_LABEL;
use super::rollups::is_pending;
use super::table::pct_cell;
use super::*;

/// A device-level rollup for the reboot view, compliance computation, the
/// workbook's Devices sheet and the device drill-down.
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
    pub offline: bool,
    /// Whether this device is in the population every fleet-health rollup describes
    /// ([`rollup_device`]), and if not, why. Carried per device so the Devices sheet
    /// and the drill-down can say *which* devices the scope note's "N offline and M
    /// non-patchable devices excluded" means, rather than leaving the reader to infer
    /// it from an Online column and a node class.
    pub rollup_scope: RollupScope,
    /// Pending current patches ([`is_pending`]) on this device, by severity band.
    /// Sums to `pending_count`; filled by [`apply_device_health`].
    pub pending_by_severity: SeverityCounts,
    /// Pending Critical/Important patches first seen longer ago than the SLA window
    /// (or undated) — [`ComplianceBucket::aged_critical`] for one device.
    pub aged_critical: usize,
    /// FAILED install-history records for this device inside the lookback window,
    /// or `None` when the query did not ask for the Failed status and so never
    /// fetched them — blank rather than zero, because "no failures" is not something
    /// such a result can know.
    pub failed_installs: Option<usize>,
    /// When the agent last checked in ([`Device::last_contact`]), formatted like the
    /// row dates, plus the instant for the workbook's date cell.
    pub last_contact: Option<String>,
    pub last_contact_ts: Option<i64>,
}

/// Whether a device counts toward the fleet-health rollups — [`rollup_device`]'s
/// verdict, with the reason when it does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RollupScope {
    Included,
    /// It reports no current patch records, so a zero pending count says nothing.
    Offline,
    /// Online, but not something NinjaOne patch management covers.
    NonPatchable,
}

impl RollupScope {
    fn of(d: &Device) -> Self {
        if d.is_offline() {
            Self::Offline
        } else if !d.is_patchable() {
            Self::NonPatchable
        } else {
            Self::Included
        }
    }

    /// The Devices sheet's wording: an exclusion names its reason.
    pub fn label(self) -> &'static str {
        match self {
            Self::Included => "Included",
            Self::Offline => "Excluded (offline)",
            Self::NonPatchable => "Excluded (non-patchable)",
        }
    }
}

pub fn build_device_summaries(
    devices: &[&Device],
    pending_counts: &HashMap<i64, usize>,
    maps: &LookupMaps,
) -> Vec<DeviceSummary> {
    devices
        .iter()
        .map(|d| {
            let last_contact = d.last_contact_at();
            DeviceSummary {
                device_id: d.id,
                device_name: d.label().to_string(),
                organization: maps.org_name(d.organization_id),
                location: maps.location_name(d.location_id),
                device_role: maps.role_name(d.node_role_id),
                os_name: d.os_name(),
                node_class: d.node_class.clone(),
                needs_reboot: d.needs_reboot(),
                pending_count: pending_counts.get(&d.id).copied().unwrap_or(0),
                offline: d.is_offline(),
                rollup_scope: RollupScope::of(d),
                pending_by_severity: SeverityCounts::default(),
                aged_critical: 0,
                failed_installs: None,
                last_contact: super::join::fmt_dt(last_contact),
                last_contact_ts: last_contact.map(|t| t.timestamp()),
            }
        })
        .collect()
}

/// Fills each summary's per-device health — pending by severity band, pending past
/// SLA and, when they were fetched, failed installs — from the same unnarrowed
/// current feed the compliance rollups read, through the same [`is_pending`],
/// backlog and SLA predicates. One function, so the Devices sheet and the
/// drill-down cannot grade a device differently from the Compliance sheet that
/// counts it.
///
/// `failed_installs` is the FAILED install-history records, or `None` when the query
/// did not request the Failed status; the summaries then keep `None` rather than
/// claiming zero. A patch whose device is not in `summaries` is ignored.
pub fn apply_device_health(
    summaries: &mut [DeviceSummary],
    current_patches: &[&Patch],
    failed_installs: Option<&[&Patch]>,
    sla_days: i64,
    now: DateTime<Utc>,
) {
    let index: HashMap<i64, usize> = summaries
        .iter()
        .enumerate()
        .map(|(i, s)| (s.device_id, i))
        .collect();
    let slot = |id: Option<i64>| id.and_then(|id| index.get(&id).copied());
    let sla_cutoff = now - Duration::days(sla_days);
    for p in current_patches {
        let Some(i) = slot(p.device_id) else {
            continue;
        };
        if !is_pending(p.status.as_deref()) {
            continue;
        }
        let s = &mut summaries[i];
        s.pending_by_severity.add(p.severity_enum());
        if counts_toward_backlog(p) && is_aged(p, sla_cutoff) {
            s.aged_critical += 1;
        }
    }
    if let Some(failed) = failed_installs {
        let mut counts = vec![0usize; summaries.len()];
        for p in failed {
            if let Some(i) = slot(p.device_id) {
                counts[i] += 1;
            }
        }
        for (s, n) in summaries.iter_mut().zip(counts) {
            s.failed_installs = Some(n);
        }
    }
}

/// The order the Devices sheet and the reboot list read in: organization, then
/// device name (both case-insensitive), then id — so two devices sharing a name
/// still list the same way every run.
pub fn sort_device_summaries(summaries: &mut [DeviceSummary]) {
    summaries.sort_by(|a, b| {
        cmp_ci(&a.organization, &b.organization)
            .then_with(|| cmp_ci(&a.device_name, &b.device_name))
            .then_with(|| a.device_id.cmp(&b.device_id))
    });
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
    /// Pending Critical/Important patches first seen longer ago than the SLA
    /// window — the backlog that has aged past target.
    pub aged_critical: usize,
}

/// One compliance bucket under construction, keyed by whatever the caller groups on.
#[derive(Default)]
struct ComplianceAcc {
    total: usize,
    compliant: usize,
    pending_critical: usize,
    aged_critical: usize,
}

impl ComplianceAcc {
    /// Compliant share. An empty bucket is 100%, not 0% — it has no devices to be
    /// non-compliant.
    fn pct(&self) -> f64 {
        if self.total == 0 {
            100.0
        } else {
            (self.compliant as f64 / self.total as f64) * 100.0
        }
    }
}

/// Whether a current-patch record belongs to the backlog the compliance rollups
/// track: not yet installed, and at least Important.
///
/// Pending is [`is_pending`]'s exclude list — every status except `REJECTED` and
/// `INSTALLED` counts, not only NinjaOne's usual `MANUAL`/`APPROVED`. The rank
/// threshold deliberately excludes `Security` and `Recommended`, which are NinjaOne
/// *classifications* rather than urgency grades.
fn counts_toward_backlog(p: &Patch) -> bool {
    is_pending(p.status.as_deref()) && p.severity_enum().rank() >= Severity::Important.rank()
}

/// Whether a pending patch has aged past the SLA cutoff.
///
/// A patch NinjaOne has never timestamped can't be proven recent, so it is flagged
/// for review rather than assumed within SLA (which would understate the backlog).
fn is_aged(p: &Patch, sla_cutoff: DateTime<Utc>) -> bool {
    p.first_seen_at().map(|r| r < sla_cutoff).unwrap_or(true)
}

/// The device a fleet-health rollup should attribute a patch to, or `None` when the
/// patch falls outside the population every one of those rollups describes: the
/// device must be in the scoped inventory **and** online.
///
/// One predicate for compliance, the severity breakdown and the age histogram, so
/// they cannot describe three different populations of the same fleet. They did:
/// only [`accumulate_compliance`] applied this, so a report whose header states
/// "Compliance covers online devices only (N offline devices excluded)" printed
/// those very devices' backlog in the severity and age charts directly beneath the
/// sentence — the gap being exactly the excluded population, and invisible. The
/// inventory half matters for the same reason: an orphan patch (no matching device,
/// which only survives scoping when no identity facet is active) used to open its
/// own `(unknown)` organization in the severity breakdown while compliance dropped
/// it, so the two disagreed about how many organizations the fleet even had.
///
/// An offline device can't apply patches and reports no current patch records, so a
/// zero pending count says nothing about its compliance: it is excluded from the
/// denominator rather than scored compliant and inflating the headline metric. A
/// device NinjaOne cannot patch at all ([`Device::is_patchable`] — switches,
/// printers, hypervisors, cloud monitors) is excluded for the same reason: it is
/// online, carries no patch records, and used to score compliant, so a fleet of
/// 100 servers and 100 network devices read 25 points better than its servers.
pub(super) fn rollup_device<'a>(
    devices_by_id: &HashMap<i64, &'a Device>,
    device_id: Option<i64>,
) -> Option<&'a Device> {
    devices_by_id
        .get(&device_id?)
        .copied()
        .filter(|d| !d.is_offline() && d.is_patchable())
}

/// The body shared by [`build_compliance`] and [`build_compliance_by_os`], which
/// differ only in what they group by.
///
/// Factored out because the two were ~72 lines of near-verbatim copy — the offline
/// exclusion, the pending predicate, the Important threshold, the SLA cutoff and the
/// percentage — so any change to a compliance rule had to be made twice or the
/// Compliance and By-OS tabs would quietly disagree about the same fleet.
///
/// Keys are `Cow` so a grouping that can borrow (OS name, device summary fields)
/// allocates only when a bucket is first created, rather than once per patch.
fn accumulate_compliance<'a>(
    summaries: &'a [DeviceSummary],
    current_patches: &'a [&Patch],
    devices_by_id: &HashMap<i64, &'a Device>,
    sla_days: i64,
    now: DateTime<Utc>,
    device_key: impl Fn(&'a DeviceSummary) -> Cow<'a, str>,
    patch_key: impl Fn(Option<&'a Device>) -> Cow<'a, str>,
) -> HashMap<String, ComplianceAcc> {
    let mut by_key: HashMap<String, ComplianceAcc> = HashMap::new();

    // [`rollup_device`] is the one rule for who is in this rollup at all, applied to
    // *both* halves below.
    let counted = |device_id: Option<i64>| rollup_device(devices_by_id, device_id).is_some();

    for s in summaries {
        if !counted(Some(s.device_id)) {
            continue;
        }
        let key = device_key(s);
        let acc = match by_key.get_mut(key.as_ref()) {
            Some(acc) => acc,
            None => by_key.entry(key.into_owned()).or_default(),
        };
        acc.total += 1;
        if s.pending_count == 0 {
            acc.compliant += 1;
        }
    }

    let sla_cutoff = now - Duration::days(sla_days);
    for p in current_patches {
        let Some(device) = rollup_device(devices_by_id, p.device_id) else {
            continue;
        };
        if !counts_toward_backlog(p) {
            continue;
        }
        let key = patch_key(Some(device));
        let acc = match by_key.get_mut(key.as_ref()) {
            Some(acc) => acc,
            None => by_key.entry(key.into_owned()).or_default(),
        };
        acc.pending_critical += 1;
        if is_aged(p, sla_cutoff) {
            acc.aged_critical += 1;
        }
    }

    by_key
}

/// Computes per-org compliance from device summaries and the current (pending/
/// approved) patches. `sla_days` flags aged Critical/Important backlog.
pub fn build_compliance(
    summaries: &[DeviceSummary],
    current_patches: &[&Patch],
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
    sla_days: i64,
    now: DateTime<Utc>,
) -> Vec<ComplianceBucket> {
    let by_org = accumulate_compliance(
        summaries,
        current_patches,
        devices_by_id,
        sla_days,
        now,
        |s| Cow::Borrowed(s.organization.as_str()),
        |d| Cow::Borrowed(maps.org_name_str(d.and_then(|d| d.organization_id))),
    );

    let mut buckets: Vec<ComplianceBucket> = by_org
        .into_iter()
        .map(|(organization, a)| ComplianceBucket {
            organization,
            devices_total: a.total,
            devices_compliant: a.compliant,
            compliance_pct: a.pct(),
            pending_critical: a.pending_critical,
            aged_critical: a.aged_critical,
        })
        .collect();
    buckets.sort_by_cached_key(|b| b.organization.to_lowercase());
    buckets
}

/// Per-OS compliance rollup (grouped by the device's reported OS name) for the
/// Compliance tab's "Compliance by OS" section. Same shape as [`ComplianceBucket`]
/// but keyed on OS instead of organization.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsCompliance {
    pub os: String,
    pub devices_total: usize,
    pub devices_compliant: usize,
    pub compliance_pct: f64,
    pub pending_critical: usize,
    pub aged_critical: usize,
}

/// Computes compliance grouped by OS name, mirroring [`build_compliance`] (offline
/// devices excluded from the denominator; pending Critical/Important counted, and
/// flagged aged when older than the SLA window or undated). Devices and patches with
/// no reported OS fall under "(unknown)".
pub fn build_compliance_by_os(
    summaries: &[DeviceSummary],
    current_patches: &[&Patch],
    devices_by_id: &HashMap<i64, &Device>,
    sla_days: i64,
    now: DateTime<Utc>,
) -> Vec<OsCompliance> {
    let by_os = accumulate_compliance(
        summaries,
        current_patches,
        devices_by_id,
        sla_days,
        now,
        |s| Cow::Borrowed(s.os_name.as_deref().unwrap_or(UNKNOWN_LABEL)),
        |d| Cow::Borrowed(d.and_then(|d| d.os_name_str()).unwrap_or(UNKNOWN_LABEL)),
    );

    let mut buckets: Vec<OsCompliance> = by_os
        .into_iter()
        .map(|(os, a)| OsCompliance {
            os,
            devices_total: a.total,
            devices_compliant: a.compliant,
            compliance_pct: a.pct(),
            pending_critical: a.pending_critical,
            aged_critical: a.aged_critical,
        })
        .collect();
    buckets.sort_by_cached_key(|b| b.os.to_lowercase());
    buckets
}

/// Counts current pending patches ([`is_pending`] — anything not `REJECTED` or
/// `INSTALLED`) per device for compliance and the reboot/summary views. Shares [`is_pending`] with every other pending rollup, so
/// the device's pending count, the severity breakdown and the age histogram cannot
/// disagree about which records are pending.
pub fn pending_counts(current_patches: &[&Patch]) -> HashMap<i64, usize> {
    let mut counts: HashMap<i64, usize> = HashMap::new();
    for p in current_patches {
        if is_pending(p.status.as_deref())
            && let Some(id) = p.device_id
        {
            *counts.entry(id).or_default() += 1;
        }
    }
    counts
}

impl DeviceSummary {
    /// The needs-reboot table columns. Shared by the Excel exporter and the HTML
    /// report, which previously hardcoded its own `<th>` row and had already
    /// diverged in wording ("Role" vs "Device Role", "Pending patches" vs "Pending
    /// Patches") from the workbook it is meant to mirror.
    pub const COLUMNS: [TableColumn<DeviceSummary>; 6] = [
        ("Organization", |d| TableCell::Text(d.organization.clone())),
        ("Location", |d| {
            TableCell::Text(d.location.clone().unwrap_or_default())
        }),
        ("Device Role", |d| {
            TableCell::Text(d.device_role.clone().unwrap_or_default())
        }),
        ("Device", |d| TableCell::Text(d.device_name.clone())),
        ("OS", |d| {
            TableCell::Text(d.os_name.clone().unwrap_or_default())
        }),
        ("Pending Patches", |d| TableCell::Count(d.pending_count)),
    ];

    /// The workbook's Devices sheet: one row per in-scope device, with its
    /// rollup status and its share of every fleet-health number.
    ///
    /// The count columns are blank — not zero — for a device outside the
    /// [`rollup_device`] population: an offline or non-patchable device reports no
    /// current patch records, so a 0 there would read as "clean" when it means
    /// "unknown", which is the exact misreading the exclusion exists to prevent.
    /// The band columns are one per [`SeverityCounts::BANDS`] entry, in its order.
    pub const DEVICE_COLUMNS: [TableColumn<DeviceSummary>; 19] = [
        ("Organization", |d| TableCell::text(&d.organization)),
        ("Location", |d| TableCell::opt_text(d.location.as_deref())),
        ("Device Role", |d| {
            TableCell::opt_text(d.device_role.as_deref())
        }),
        ("Device", |d| TableCell::text(&d.device_name)),
        ("OS", |d| TableCell::opt_text(d.os_name.as_deref())),
        ("Online", |d| {
            TableCell::text(if d.offline { "No" } else { "Yes" })
        }),
        ("Compliance Scope", |d| {
            TableCell::text(d.rollup_scope.label())
        }),
        (DEVICE_BAND_HEADERS[0], band_cell::<0>),
        (DEVICE_BAND_HEADERS[1], band_cell::<1>),
        (DEVICE_BAND_HEADERS[2], band_cell::<2>),
        (DEVICE_BAND_HEADERS[3], band_cell::<3>),
        (DEVICE_BAND_HEADERS[4], band_cell::<4>),
        (DEVICE_BAND_HEADERS[5], band_cell::<5>),
        (DEVICE_BAND_HEADERS[6], band_cell::<6>),
        (DEVICE_BAND_HEADERS[7], band_cell::<7>),
        ("Aged (past SLA)", |d| included_count(d, d.aged_critical)),
        ("Failed Installs", |d| match d.failed_installs {
            Some(n) => TableCell::Count(n),
            None => TableCell::Text(String::new()),
        }),
        ("Needs Reboot", |d| {
            TableCell::text(if d.needs_reboot { "Yes" } else { "No" })
        }),
        ("Last Contact", |d| TableCell::DateTime(d.last_contact_ts)),
    ];
}

/// Headers of the Devices sheet's per-band columns, index-aligned with
/// [`SeverityCounts::BANDS`] (`device_band_headers_follow_the_bands` pins it — a
/// `const` cannot concatenate the band labels itself).
pub const DEVICE_BAND_HEADERS: [&str; SeverityCounts::BANDS.len()] = [
    "Pending Critical",
    "Pending Important",
    "Pending Security",
    "Pending Moderate",
    "Pending Recommended",
    "Pending Low",
    "Pending Optional",
    "Pending Unknown",
];

/// One band's pending count for [`DeviceSummary::DEVICE_COLUMNS`], read through
/// [`SeverityCounts::BANDS`] rather than a named field so the band list stays the
/// one enumeration.
fn band_cell<const BAND: usize>(d: &DeviceSummary) -> TableCell {
    included_count(d, (SeverityCounts::BANDS[BAND].1)(&d.pending_by_severity))
}

/// A rollup count for a device in the rollup population, blank for one outside it.
fn included_count(d: &DeviceSummary, n: usize) -> TableCell {
    match d.rollup_scope {
        RollupScope::Included => TableCell::Count(n),
        RollupScope::Offline | RollupScope::NonPatchable => TableCell::Text(String::new()),
    }
}

impl ComplianceBucket {
    /// The per-organization compliance columns.
    pub const COLUMNS: [TableColumn<ComplianceBucket>; 6] = [
        ("Organization", |b| TableCell::Text(b.organization.clone())),
        ("Devices", |b| TableCell::Count(b.devices_total)),
        ("Compliant", |b| TableCell::Count(b.devices_compliant)),
        ("Compliance %", |b| pct_cell(b.compliance_pct)),
        ("Pending Critical/Important", |b| {
            TableCell::Count(b.pending_critical)
        }),
        ("Aged (past SLA)", |b| TableCell::Count(b.aged_critical)),
    ];
}

impl OsCompliance {
    /// The per-OS compliance columns. Same shape as [`ComplianceBucket::COLUMNS`]
    /// apart from the leading identity column.
    pub const COLUMNS: [TableColumn<OsCompliance>; 6] = [
        ("OS", |b| TableCell::Text(b.os.clone())),
        ("Devices", |b| TableCell::Count(b.devices_total)),
        ("Compliant", |b| TableCell::Count(b.devices_compliant)),
        ("Compliance %", |b| pct_cell(b.compliance_pct)),
        ("Pending Critical/Important", |b| {
            TableCell::Count(b.pending_critical)
        }),
        ("Aged (past SLA)", |b| TableCell::Count(b.aged_critical)),
    ];
}

/// Which current-patch families a query's fleet-health rollups were computed from.
///
/// Not a copy of the patch-type facet for its own sake: it is the honest scope of
/// every compliance/severity/age number, and the one thing that cannot be recovered
/// from the numbers themselves. See [`QueryResult::patch_families`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchFamilies {
    pub os: bool,
    pub software: bool,
}

impl PatchFamilies {
    /// The operator-facing name of this scope, for a label that has room for it.
    pub fn label(self) -> &'static str {
        match (self.os, self.software) {
            (true, true) => "OS and third-party patches",
            (true, false) => "OS patches only",
            (false, true) => "third-party patches only",
            // Not reachable from the UI (the Type facet is ALL/OS/SOFTWARE), but a
            // rollup over nothing must not silently read as a rollup over
            // everything.
            (false, false) => "no patch families",
        }
    }

    /// Whether both families were included, i.e. whether the rollups describe the
    /// whole backlog. The UI shows a scope note when they don't.
    pub fn is_complete(self) -> bool {
        self.os && self.software
    }
}

/// One sentence stating exactly which devices and which patches the fleet-health
/// rollups in this result describe.
///
/// Every surface that shows a compliance number prints this beside it — the
/// Compliance tab, the HTML report header and the workbook's Compliance sheet — so
/// none of them can imply a scope the numbers don't have. Two things are invisible
/// in a bare percentage and both change what it means: offline devices are excluded
/// from the rollups entirely, and only the patch families the query fetched are in
/// them.
pub fn compliance_scope_note(
    devices_offline: usize,
    devices_unpatchable: usize,
    families: PatchFamilies,
) -> String {
    let excluded = excluded_clause(devices_offline, devices_unpatchable);
    let families = if families.is_complete() {
        String::new()
    } else {
        format!(", and counts {}", families.label())
    };
    format!("Compliance covers online Windows, macOS and Linux devices only{excluded}{families}.")
}

/// The parenthetical naming the devices [`rollup_device`] left out, so the reader
/// can reconcile the compliance table's `Devices` column with `devices_total`:
/// `devices_total − offline − non-patchable` is the denominator. Empty when nothing
/// was excluded. Mirrored in `web-rs/src/app/util/format.rs`.
fn excluded_clause(offline: usize, unpatchable: usize) -> String {
    let devices = |n: usize| if n == 1 { "device" } else { "devices" };
    match (offline, unpatchable) {
        (0, 0) => String::new(),
        (n, 0) => format!(" ({n} offline {} excluded)", devices(n)),
        (0, m) => format!(" ({m} non-patchable {} excluded)", devices(m)),
        (n, m) => format!(
            " ({n} offline and {m} non-patchable {} excluded)",
            devices(n + m)
        ),
    }
}
