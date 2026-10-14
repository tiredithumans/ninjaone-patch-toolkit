//! Fleet-health rollups over the scoped inventory: per-device summaries (the
//! Needs-Reboot view), compliance by organization and by OS, pending counts,
//! the `rollup_device` population every rollup shares, and the scope note that
//! states what that population excludes.

use std::borrow::Cow;
use std::collections::HashMap;

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;

use crate::model::{Device, Patch, PatchStatus, Severity};
use crate::settings::SlaPolicy;

use super::join::UNKNOWN_LABEL;
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
    /// Carried so the Needs Reboot tab's device selection can count offline targets
    /// before planning — an action against an offline device is queued, not run.
    pub offline: bool,
    pub pending_count: usize,
    /// Whether this device is in the population every fleet-health rollup describes
    /// ([`rollup_device`]), and if not, why. Carried per device so the Devices sheet
    /// and the drill-down can say *which* devices the scope note's "N offline and M
    /// non-patchable devices excluded" means, rather than leaving the reader to infer
    /// it from an Online column and a node class.
    pub rollup_scope: RollupScope,
    /// Pending current patches ([`is_pending`](super::rollups::is_pending)) on this
    /// device, by severity band. Sums to `pending_count`.
    pub pending_by_severity: SeverityCounts,
    /// Pending patches of **any** band past that band's own SLA window (or undated);
    /// a band with No SLA never counts. Zero is what makes a counted device Within
    /// SLA — the same predicate, over the same records, as the worst-devices list's
    /// "Past SLA".
    pub past_sla: usize,
    /// Pending urgent (Critical, Important, Security) patches first seen longer ago
    /// than their SLA window (or undated) — [`ComplianceBucket::urgent_past_sla`] for
    /// one device.
    pub urgent_past_sla: usize,
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

/// One summary per in-scope device, with its share of the fleet-health numbers —
/// pending count, pending by severity band, pending past SLA and, when they were
/// fetched, failed installs — from the same unnarrowed current feed the compliance
/// rollups read, through the same pending, backlog and SLA predicates. One
/// function, so the Devices sheet and the drill-down cannot grade a device
/// differently from the Compliance sheet that counts it, and so a summary cannot be
/// built without its health filled in: the compliance rollups read these counts,
/// and an unfilled summary would read as a clean device.
///
/// `failed_installs` is the FAILED install-history records, or `None` when the query
/// did not request the Failed status; the summaries then keep `None` rather than
/// claiming zero. A record whose device is not in `devices` is ignored.
pub fn build_device_summaries(
    devices: &[&Device],
    current: &[PatchRecord<'_>],
    failed_installs: Option<&[&Patch]>,
    sla: &SlaCutoffs,
    maps: &LookupMaps,
) -> Vec<DeviceSummary> {
    let mut summaries: Vec<DeviceSummary> = devices
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
                pending_count: 0,
                offline: d.is_offline(),
                rollup_scope: RollupScope::of(d),
                pending_by_severity: SeverityCounts::default(),
                past_sla: 0,
                urgent_past_sla: 0,
                failed_installs: None,
                last_contact: super::join::fmt_dt(last_contact),
                last_contact_ts: last_contact.map(|t| t.timestamp()),
            }
        })
        .collect();
    let index: HashMap<i64, usize> = summaries
        .iter()
        .enumerate()
        .map(|(i, s)| (s.device_id, i))
        .collect();
    let slot = |id: Option<i64>| id.and_then(|id| index.get(&id).copied());
    for r in current {
        if !r.pending {
            continue;
        }
        let Some(i) = slot(r.patch.device_id) else {
            continue;
        };
        let s = &mut summaries[i];
        s.pending_count += 1;
        s.pending_by_severity.add(r.severity);
        if sla.is_aged(r) {
            s.past_sla += 1;
            if counts_toward_backlog(r) {
                s.urgent_past_sla += 1;
            }
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
    summaries
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
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComplianceBucket {
    pub organization: String,
    /// Counted devices: in scope, online and patchable ([`rollup_device`]).
    pub devices_total: usize,
    /// Counted devices with no pending patch past its band's SLA window
    /// ([`DeviceSummary::past_sla`] is zero) — the headline figure.
    pub devices_within_sla: usize,
    pub within_sla_pct: f64,
    /// Counted devices with no pending patch at all, of any severity or age.
    pub devices_fully_patched: usize,
    pub fully_patched_pct: f64,
    /// Pending urgent patches — Critical, Important or Security
    /// ([`Severity::is_urgent`]).
    pub urgent_pending: usize,
    /// The subset of `urgent_pending` first seen longer ago than its band's SLA
    /// window — the urgent backlog that has aged past target.
    pub urgent_past_sla: usize,
    /// Patches, of any severity, NinjaOne holds for an approval decision (vendor
    /// status `MANUAL`) — waiting on a person. See [`approval_state`].
    pub awaiting_approval: usize,
    /// Patches, of any severity, already `APPROVED` and still not installed —
    /// waiting on the agent.
    pub approved_not_installed: usize,
}

/// One compliance bucket under construction, keyed by whatever the caller groups on.
#[derive(Default)]
struct ComplianceAcc {
    total: usize,
    within_sla: usize,
    fully_patched: usize,
    urgent_pending: usize,
    urgent_past_sla: usize,
    awaiting_approval: usize,
    approved_not_installed: usize,
}

/// Where a current-feed record stands in NinjaOne's approval workflow, read from
/// the **vendor's** `status` on the cached [`Patch`] — never from a row's display
/// status.
///
/// The distinction matters because the two ends of the pipeline stall for
/// different reasons: `MANUAL` (NinjaOne's "Pending", i.e. pending approval) is
/// waiting on a person, `APPROVED` is waiting on the agent. Both count as pending
/// in [`is_pending`](super::rollups::is_pending), so the compliance percentage cannot tell them apart.
///
/// Deliberately *not* the row status. `assemble_result` gives the current sources
/// `status_override = MANUAL` so an untyped record matches the Pending selection
/// and renders as PENDING — a display decision. Reading that here would file every
/// untyped record as "awaiting approval", a claim the vendor never made. The
/// rollups take the raw cached records (the override only ever reaches
/// `build_rows`), so an untyped, `FAILED` or never-seen status is pending but in
/// **neither** column, and the two columns can sum to less than the pending count.
/// Compared exactly, like [`is_pending`](super::rollups::is_pending): `status` is free-form in the spec, and
/// this crate has only ever seen it upper-case.
pub(super) fn approval_state(status: Option<&str>) -> Option<ApprovalState> {
    match status.and_then(PatchStatus::from_api_value) {
        Some(PatchStatus::Pending) => Some(ApprovalState::Awaiting),
        Some(PatchStatus::Approved) => Some(ApprovalState::Approved),
        _ => None,
    }
}

/// See [`approval_state`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ApprovalState {
    /// `MANUAL`: pending an approval decision.
    Awaiting,
    /// `APPROVED`: approved, not yet installed.
    Approved,
}

/// `n` as a percentage of `total`. An empty bucket is 100%, not 0% — it has no
/// devices to fall short — which no bucket built from devices can be; the fleet
/// total over an empty scope can, and every surface prints "—" for it instead.
fn share(n: usize, total: usize) -> f64 {
    if total == 0 {
        100.0
    } else {
        n as f64 * 100.0 / total as f64
    }
}

/// Whether a current-patch record belongs to the urgent backlog the compliance
/// rollups track: not yet installed, and Critical, Important or Security
/// ([`Severity::is_urgent`]).
///
/// Pending is [`is_pending`](super::rollups::is_pending)'s exclude list — every
/// status except `REJECTED` and `INSTALLED` counts, not only NinjaOne's usual
/// `MANUAL`/`APPROVED`.
fn counts_toward_backlog(r: &PatchRecord<'_>) -> bool {
    r.pending && r.severity.is_urgent()
}

/// The SLA cutoff for every severity band at one instant: a record first seen
/// before its band's cutoff is past SLA. A band held to no SLA has no cutoff.
///
/// Computed once per query rather than once per patch, indexed by
/// [`Severity::rank`] and sized from [`Severity::ALL`], which lists every variant at
/// its rank, so every slot is filled. Each band's window comes from
/// [`SlaPolicy::window_for`] — its override, else the default, or none at all for a
/// band switched to No SLA; `Unknown` always takes the default.
pub struct SlaCutoffs([Option<DateTime<Utc>>; Severity::ALL.len()]);

impl SlaCutoffs {
    pub fn new(policy: &SlaPolicy, now: DateTime<Utc>) -> Self {
        let mut cutoffs = [None; Severity::ALL.len()];
        for severity in Severity::ALL {
            cutoffs[severity.rank() as usize] = policy
                .window_for(severity)
                .map(|days| now - Duration::days(days));
        }
        Self(cutoffs)
    }

    /// The band's cutoff, or `None` for a band held to no SLA.
    pub fn cutoff_for(&self, severity: Severity) -> Option<DateTime<Utc>> {
        self.0[severity.rank() as usize]
    }

    /// Whether a pending record has aged past its band's SLA cutoff, measured from
    /// its effective first seen ([`PatchRecord::first_seen`]).
    ///
    /// A record nothing has dated can't be proven recent, so it is flagged for
    /// review rather than assumed within SLA (which would understate the backlog).
    /// A band held to no SLA is never past it — undated or not: there is no target
    /// for it to have missed.
    pub fn is_aged(&self, r: &PatchRecord<'_>) -> bool {
        let Some(cutoff) = self.cutoff_for(r.severity) else {
            return false;
        };
        r.first_seen.map(|t| t < cutoff).unwrap_or(true)
    }
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
    current: &[PatchRecord<'a>],
    devices_by_id: &HashMap<i64, &'a Device>,
    sla: &SlaCutoffs,
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
        if s.past_sla == 0 {
            acc.within_sla += 1;
        }
        if s.pending_count == 0 {
            acc.fully_patched += 1;
        }
    }

    for r in current {
        let Some(device) = rollup_device(devices_by_id, r.patch.device_id) else {
            continue;
        };
        // The approval split counts every severity (it describes the workflow, not
        // the urgency); the SLA backlog only the urgent bands.
        let approval = approval_state(r.patch.status.as_deref());
        let backlog = counts_toward_backlog(r);
        if approval.is_none() && !backlog {
            continue;
        }
        let key = patch_key(Some(device));
        let acc = match by_key.get_mut(key.as_ref()) {
            Some(acc) => acc,
            None => by_key.entry(key.into_owned()).or_default(),
        };
        match approval {
            Some(ApprovalState::Awaiting) => acc.awaiting_approval += 1,
            Some(ApprovalState::Approved) => acc.approved_not_installed += 1,
            None => {}
        }
        if backlog {
            acc.urgent_pending += 1;
            if sla.is_aged(r) {
                acc.urgent_past_sla += 1;
            }
        }
    }

    by_key
}

/// Computes per-org compliance from device summaries and the current (pending/
/// approved) patches. `sla` flags aged urgent backlog, each patch
/// against its own band's cutoff.
pub fn build_compliance(
    summaries: &[DeviceSummary],
    current: &[PatchRecord<'_>],
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
    sla: &SlaCutoffs,
) -> Vec<ComplianceBucket> {
    let by_org = accumulate_compliance(
        summaries,
        current,
        devices_by_id,
        sla,
        |s| Cow::Borrowed(s.organization.as_str()),
        |d| Cow::Borrowed(maps.org_name_str(d.and_then(|d| d.organization_id))),
    );

    let mut buckets: Vec<ComplianceBucket> = by_org
        .into_iter()
        .map(|(organization, a)| ComplianceBucket {
            organization,
            devices_total: a.total,
            devices_within_sla: a.within_sla,
            within_sla_pct: share(a.within_sla, a.total),
            devices_fully_patched: a.fully_patched,
            fully_patched_pct: share(a.fully_patched, a.total),
            urgent_pending: a.urgent_pending,
            urgent_past_sla: a.urgent_past_sla,
            awaiting_approval: a.awaiting_approval,
            approved_not_installed: a.approved_not_installed,
        })
        .collect();
    buckets.sort_by_cached_key(|b| b.organization.to_lowercase());
    buckets
}

impl ComplianceBucket {
    /// The label the fleet-wide total carries in its organization column.
    pub const TOTAL_LABEL: &'static str = "All organizations";

    /// The fleet-wide figures: every organization's counts summed and both
    /// percentages recomputed from the sums, so the total is device-weighted — a
    /// 2-device org at 50% does not pull a 200-device org at 100% down to 75%. Exact,
    /// because every counted device and record lands in exactly one organization
    /// (both halves of [`accumulate_compliance`] key by the same org-name lookup).
    pub fn total(buckets: &[ComplianceBucket]) -> Self {
        let sum = |f: fn(&ComplianceBucket) -> usize| buckets.iter().map(f).sum::<usize>();
        let devices_total = sum(|b| b.devices_total);
        let devices_within_sla = sum(|b| b.devices_within_sla);
        let devices_fully_patched = sum(|b| b.devices_fully_patched);
        Self {
            organization: Self::TOTAL_LABEL.to_string(),
            devices_total,
            devices_within_sla,
            within_sla_pct: share(devices_within_sla, devices_total),
            devices_fully_patched,
            fully_patched_pct: share(devices_fully_patched, devices_total),
            urgent_pending: sum(|b| b.urgent_pending),
            urgent_past_sla: sum(|b| b.urgent_past_sla),
            awaiting_approval: sum(|b| b.awaiting_approval),
            approved_not_installed: sum(|b| b.approved_not_installed),
        }
    }
}

/// Organizations with devices in scope but none counted — every one offline or not
/// patchable — so they have no row in the compliance table at all. Named so every
/// surface can say where they went rather than let an organization silently vanish.
/// Case-insensitively sorted, like the table.
pub fn uncounted_orgs(summaries: &[DeviceSummary]) -> Vec<String> {
    let mut counted: HashMap<&str, bool> = HashMap::new();
    for s in summaries {
        *counted.entry(s.organization.as_str()).or_default() |=
            s.rollup_scope == RollupScope::Included;
    }
    let mut out: Vec<String> = counted
        .into_iter()
        .filter(|(_, any)| !any)
        .map(|(org, _)| org.to_string())
        .collect();
    out.sort_by(|a, b| cmp_ci(a, b));
    out
}

/// Per-OS compliance rollup (grouped by the device's reported OS name) for the
/// Compliance tab's "Compliance by OS" section. Same shape as [`ComplianceBucket`]
/// but keyed on OS instead of organization.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OsCompliance {
    pub os: String,
    pub devices_total: usize,
    pub devices_within_sla: usize,
    pub within_sla_pct: f64,
    pub devices_fully_patched: usize,
    pub fully_patched_pct: f64,
    pub urgent_pending: usize,
    pub urgent_past_sla: usize,
    pub awaiting_approval: usize,
    pub approved_not_installed: usize,
}

/// Computes compliance grouped by OS name, mirroring [`build_compliance`] (offline
/// devices excluded from the denominator; pending urgent patches counted, and
/// flagged aged when older than the SLA window or undated). Devices and patches with
/// no reported OS fall under "(unknown)".
pub fn build_compliance_by_os(
    summaries: &[DeviceSummary],
    current: &[PatchRecord<'_>],
    devices_by_id: &HashMap<i64, &Device>,
    sla: &SlaCutoffs,
) -> Vec<OsCompliance> {
    let by_os = accumulate_compliance(
        summaries,
        current,
        devices_by_id,
        sla,
        |s| Cow::Borrowed(s.os_name.as_deref().unwrap_or(UNKNOWN_LABEL)),
        |d| Cow::Borrowed(d.and_then(|d| d.os_name_str()).unwrap_or(UNKNOWN_LABEL)),
    );

    let mut buckets: Vec<OsCompliance> = by_os
        .into_iter()
        .map(|(os, a)| OsCompliance {
            os,
            devices_total: a.total,
            devices_within_sla: a.within_sla,
            within_sla_pct: share(a.within_sla, a.total),
            devices_fully_patched: a.fully_patched,
            fully_patched_pct: share(a.fully_patched, a.total),
            urgent_pending: a.urgent_pending,
            urgent_past_sla: a.urgent_past_sla,
            awaiting_approval: a.awaiting_approval,
            approved_not_installed: a.approved_not_installed,
        })
        .collect();
    buckets.sort_by_cached_key(|b| b.os.to_lowercase());
    buckets
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
    pub const DEVICE_COLUMNS: [TableColumn<DeviceSummary>; 21] = [
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
        ("Within SLA", |d| match d.rollup_scope {
            RollupScope::Included => TableCell::text(if d.past_sla == 0 { "Yes" } else { "No" }),
            RollupScope::Offline | RollupScope::NonPatchable => TableCell::Text(String::new()),
        }),
        (DEVICE_BAND_HEADERS[0], band_cell::<0>),
        (DEVICE_BAND_HEADERS[1], band_cell::<1>),
        (DEVICE_BAND_HEADERS[2], band_cell::<2>),
        (DEVICE_BAND_HEADERS[3], band_cell::<3>),
        (DEVICE_BAND_HEADERS[4], band_cell::<4>),
        (DEVICE_BAND_HEADERS[5], band_cell::<5>),
        (DEVICE_BAND_HEADERS[6], band_cell::<6>),
        (DEVICE_BAND_HEADERS[7], band_cell::<7>),
        ("Past SLA", |d| included_count(d, d.past_sla)),
        ("Urgent Past SLA", |d| included_count(d, d.urgent_past_sla)),
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
    pub const COLUMNS: [TableColumn<ComplianceBucket>; 10] = [
        ("Organization", |b| TableCell::Text(b.organization.clone())),
        ("Devices", |b| TableCell::Count(b.devices_total)),
        ("Within SLA", |b| TableCell::Count(b.devices_within_sla)),
        ("Within SLA %", |b| pct_cell(b.within_sla_pct)),
        ("Fully Patched", |b| {
            TableCell::Count(b.devices_fully_patched)
        }),
        ("Fully Patched %", |b| pct_cell(b.fully_patched_pct)),
        ("Urgent Pending", |b| TableCell::Count(b.urgent_pending)),
        ("Urgent Past SLA", |b| TableCell::Count(b.urgent_past_sla)),
        ("Awaiting Approval", |b| {
            TableCell::Count(b.awaiting_approval)
        }),
        ("Approved, Not Installed", |b| {
            TableCell::Count(b.approved_not_installed)
        }),
    ];
}

impl OsCompliance {
    /// The per-OS compliance columns. Same shape as [`ComplianceBucket::COLUMNS`]
    /// apart from the leading identity column.
    pub const COLUMNS: [TableColumn<OsCompliance>; 10] = [
        ("OS", |b| TableCell::Text(b.os.clone())),
        ("Devices", |b| TableCell::Count(b.devices_total)),
        ("Within SLA", |b| TableCell::Count(b.devices_within_sla)),
        ("Within SLA %", |b| pct_cell(b.within_sla_pct)),
        ("Fully Patched", |b| {
            TableCell::Count(b.devices_fully_patched)
        }),
        ("Fully Patched %", |b| pct_cell(b.fully_patched_pct)),
        ("Urgent Pending", |b| TableCell::Count(b.urgent_pending)),
        ("Urgent Past SLA", |b| TableCell::Count(b.urgent_past_sla)),
        ("Awaiting Approval", |b| {
            TableCell::Count(b.awaiting_approval)
        }),
        ("Approved, Not Installed", |b| {
            TableCell::Count(b.approved_not_installed)
        }),
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
