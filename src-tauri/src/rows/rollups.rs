//! Compact fleet-wide aggregates that ride on the summary rather than the rows:
//! install failures by patch, severity by organization, and pending-patch age
//! buckets. All three take the unnarrowed current feed and the same
//! `rollup_device` population compliance uses.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::model::{Device, Patch, PatchRow, PatchStatus, Severity, row_status};

use super::compliance::{ApprovalState, approval_state, rollup_device};
use super::join::{ORPHAN_DEVICE_ID, fmt_dt};
use super::*;

/// A fleet-wide rollup of FAILED install records grouped by patch, so the operator
/// can see which patches are failing across the most devices during a patch cycle.
/// Built from the FAILED rows already present in the result — no extra fetch.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureGroup {
    pub patch_type: &'static str,
    pub kb: Option<Arc<str>>,
    pub name: Arc<str>,
    pub severity: &'static str,
    pub severity_rank: u8,
    /// Distinct devices the patch failed on (the headline count).
    pub affected_devices: usize,
    /// Every affected device name. The in-app table shows the complete list; the
    /// Excel/HTML "Devices" cell ends in "… and N more" past Excel's cell limit.
    pub device_names: Vec<Arc<str>>,
    pub latest_failure: Option<String>,
    pub latest_failure_ts: Option<i64>,
}

impl FailureGroup {
    /// The failure-table columns as (header, accessor), in display order. Shared by
    /// the Excel exporter and the HTML report.
    pub const COLUMNS: [TableColumn<FailureGroup>; 7] = [
        ("Severity", |f| TableCell::text(f.severity)),
        ("Patch Type", |f| TableCell::text(f.patch_type)),
        ("KB", |f| TableCell::opt_text(f.kb.as_deref())),
        ("Patch", |f| TableCell::text(&f.name)),
        ("Affected Devices", |f| TableCell::Count(f.affected_devices)),
        // The instant, not its text: the workbook writes a real date-time.
        ("Latest Failure", |f| {
            TableCell::DateTime(f.latest_failure_ts)
        }),
        ("Devices", |f| {
            // Capped at Excel's per-cell limit: a patch failing on a couple of
            // thousand machines joined past it, and the one rejected cell failed the
            // whole export. The report renders the same cell, so both artifacts
            // agree; the in-app table reads `device_names` whole.
            TableCell::Text(join_capped(&f.device_names, CELL_MAX_CHARS))
        }),
    ];
}

/// One severity band: its display label and how to read that band off the counts.
pub type SeverityBand = (&'static str, fn(&SeverityCounts) -> usize);

/// Pending-patch counts by MSRC severity bucket, for the dashboard breakdown.
#[derive(Debug, Clone, Copy, Default, Serialize)]
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

impl SeverityCounts {
    /// Every band as (display label, accessor), most-to-least urgent — the same
    /// order as `Severity::rank()`, including NinjaOne's two non-MSRC
    /// classifications (`Security`, `Recommended`).
    ///
    /// This is the canonical enumeration on the counts side. Consumers derive from
    /// it instead of restating the vocabulary: the HTML report's severity chart, its
    /// legend and its denominator all read this array, so they cannot disagree about
    /// how many bands exist. The report used to match bands by *string label* with a
    /// `_ => counts.unknown` catch-all, which meant a renamed band silently reported
    /// Unknown's count and then double-counted it into the total.
    ///
    /// The labels come from [`Severity::label`] rather than being restated here, so
    /// the chart legend cannot drift from the label the table and exports print.
    pub const BANDS: [SeverityBand; Severity::ALL.len()] = [
        (Severity::Critical.label(), |c| c.critical),
        (Severity::Important.label(), |c| c.important),
        (Severity::Security.label(), |c| c.security),
        (Severity::Moderate.label(), |c| c.moderate),
        (Severity::Recommended.label(), |c| c.recommended),
        (Severity::Low.label(), |c| c.low),
        (Severity::Optional.label(), |c| c.optional),
        (Severity::Unknown.label(), |c| c.unknown),
    ];

    /// Counts one pending patch into its band. The single place a [`Severity`] maps
    /// to a field, shared by the per-organization breakdown and the per-device
    /// rollup so the two cannot file the same patch under different bands.
    pub fn add(&mut self, severity: Severity) {
        match severity {
            Severity::Critical => self.critical += 1,
            Severity::Important => self.important += 1,
            Severity::Security => self.security += 1,
            Severity::Moderate => self.moderate += 1,
            Severity::Recommended => self.recommended += 1,
            Severity::Low => self.low += 1,
            Severity::Optional => self.optional += 1,
            Severity::Unknown => self.unknown += 1,
        }
    }

    /// Total across every band. Derived from [`BANDS`](Self::BANDS) so it can never
    /// sum a different set than the charts draw.
    pub fn total(&self) -> usize {
        Self::BANDS.iter().map(|(_, get)| get(self)).sum()
    }

    /// The non-zero bands as one cell, most urgent first — `Critical 3 · Low 1`.
    /// Derived from [`BANDS`](Self::BANDS), so a table can show the whole breakdown
    /// in one column without restating the vocabulary as eight.
    pub fn breakdown(&self) -> String {
        Self::BANDS
            .iter()
            .filter_map(|(label, get)| {
                let n = get(self);
                (n > 0).then(|| format!("{label} {n}"))
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Orders two breakdowns most-urgent band first: more Criticals wins, a tie
    /// falls to Important, and so on down [`BANDS`](Self::BANDS).
    pub fn cmp_urgency(&self, other: &Self) -> std::cmp::Ordering {
        Self::BANDS
            .iter()
            .map(|(_, get)| get(self).cmp(&get(other)))
            .find(|o| o.is_ne())
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}

impl std::ops::AddAssign<&SeverityCounts> for SeverityCounts {
    /// Field-wise sum. Written out once, here, next to the struct — the one place a
    /// newly added band is hardest to miss. `total_severity_is_the_sum_of_its_bands`
    /// fails if a field is added to the struct but not to [`SeverityCounts::BANDS`].
    fn add_assign(&mut self, o: &SeverityCounts) {
        let SeverityCounts {
            critical,
            important,
            security,
            moderate,
            recommended,
            low,
            optional,
            unknown,
        } = o;
        self.critical += critical;
        self.important += important;
        self.security += security;
        self.moderate += moderate;
        self.recommended += recommended;
        self.low += low;
        self.optional += optional;
        self.unknown += unknown;
    }
}

/// A per-organization pending-patch severity breakdown for the dashboard charts.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgSeverity {
    pub organization: String,
    pub counts: SeverityCounts,
}

/// One bucket of the pending-patch age histogram (by age since first seen).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgeBucket {
    pub label: String,
    pub count: usize,
}

/// Whether a current-patch status counts toward the pending backlog.
///
/// This is an **exclude list, not an allow list**: everything in the current feed is
/// pending unless its status says the patch is no longer wanted (`REJECTED`) or is
/// already on the device (`INSTALLED`). NinjaOne uses `MANUAL` (pending approval)
/// and `APPROVED` for the common cases, but `status` has no enum in the spec and is
/// not even a required property on `DeviceOSPatch`/`DeviceSoftwarePatch` — and the
/// endpoints' own titles are "Pending, **Failed** and Rejected … report"
/// (`getPendingFailedRejected*`), so a `FAILED` record, or a value this crate has
/// never seen, can arrive here. The previous allow list (`MANUAL | APPROVED | None`)
/// treated any such record as *not* pending, which scored the device compliant and
/// dropped its most urgent patch from every rollup — the wrong direction to fail in,
/// and the opposite of what [`is_aged`] does with an undated patch. An untyped record
/// is pending for the same reason: the feed is defined as the patches with no
/// installation attempt, so absence of a status cannot mean "done".
pub(super) fn is_pending(status: Option<&str>) -> bool {
    !matches!(
        status.and_then(PatchStatus::from_api_value),
        Some(PatchStatus::Rejected | PatchStatus::Installed)
    )
}

/// Groups the FAILED detail rows by patch (`patch_type` + `kb` + `name`), counting
/// the distinct devices each failed on, the most recent failure, and the full list
/// of affected device names. Sorted by affected-device count then severity, desc,
/// with ties in first-appearance order so identical runs list identically.
pub fn build_failures(rows: &[PatchRow]) -> Vec<FailureGroup> {
    struct Acc {
        /// First-seen order in the canonical row sequence — the tie-break below.
        seq: usize,
        patch_type: &'static str,
        kb: Option<Arc<str>>,
        name: Arc<str>,
        severity: &'static str,
        severity_rank: u8,
        devices: HashSet<i64>,
        device_names: Vec<Arc<str>>,
        latest_ts: Option<i64>,
        latest_date: Option<String>,
    }
    /// patch type + KB + name — the rows' own shared strings, so grouping the
    /// failure set is refcount bumps rather than three `String` copies per row.
    type FailureKey = (&'static str, Option<Arc<str>>, Arc<str>);
    let mut groups: HashMap<FailureKey, Acc> = HashMap::new();
    for r in rows {
        if &*r.status != row_status::FAILED {
            continue;
        }
        let seq = groups.len();
        let acc = groups
            .entry((r.patch_type, r.kb.clone(), r.name.clone()))
            .or_insert_with(|| Acc {
                seq,
                patch_type: r.patch_type,
                kb: r.kb.clone(),
                name: r.name.clone(),
                severity: r.severity,
                severity_rank: r.severity_rank,
                devices: HashSet::new(),
                device_names: Vec::new(),
                latest_ts: None,
                latest_date: None,
            });
        // Count distinct devices by id, but only add a name the first time we see
        // that device, so the name list has no duplicates. A record with no device
        // id is not a device: tallying the sentinel would count every id-less
        // failure as one shared machine.
        if r.device_id != ORPHAN_DEVICE_ID && acc.devices.insert(r.device_id) {
            acc.device_names.push(r.device_name.clone());
        }
        // Surface the highest severity seen for the group (records can disagree).
        if r.severity_rank > acc.severity_rank {
            acc.severity_rank = r.severity_rank;
            acc.severity = r.severity;
        }
        if let Some(ts) = r.installed_ts
            && acc.latest_ts.map(|cur| ts > cur).unwrap_or(true)
        {
            acc.latest_ts = Some(ts);
            acc.latest_date = r.installed_date.clone();
        }
    }
    // `HashMap` iteration order is randomized per process, so without a total order
    // two identical runs listed tied patches differently — in the table, the
    // workbook and the report. Ties fall back to the order each patch first appears
    // in the canonical row sequence, which is itself deterministic.
    let mut accumulated: Vec<Acc> = groups.into_values().collect();
    accumulated.sort_unstable_by_key(|a| a.seq);
    let mut out: Vec<FailureGroup> = accumulated
        .into_iter()
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
    // Stable, so the insertion order above survives as the tie-break.
    out.sort_by_key(|g| (Reverse(g.affected_devices), Reverse(g.severity_rank)));
    out
}

/// Buckets pending current patches ([`is_pending`] — everything not `REJECTED` or
/// `INSTALLED`) by org and MSRC severity for
/// the dashboard's severity breakdown. Sorted by organization name.
pub fn build_severity_by_org(
    current_patches: &[&Patch],
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
) -> Vec<OrgSeverity> {
    // Keyed by a borrowed name. This ran `org_name` — which allocates an owned
    // `String` — once per pending record across the *whole-fleet* current-patch feed,
    // which runs to six figures, to produce at most one distinct key per organization.
    // `org_name_str` is the same lookup without the allocation, and is what the rest
    // of this file already uses for exactly this reason.
    let mut by_org: HashMap<&str, SeverityCounts> = HashMap::new();
    for p in current_patches {
        if !is_pending(p.status.as_deref()) {
            continue;
        }
        // The same population the compliance rollups describe — see
        // [`rollup_device`]. This breakdown is charted directly beneath compliance in
        // the HTML report, so counting a wider set here made the two sections
        // disagree about the fleet without saying so.
        let Some(device) = rollup_device(devices_by_id, p.device_id) else {
            continue;
        };
        let org = maps.org_name_str(device.organization_id);
        by_org.entry(org).or_default().add(p.severity_enum());
    }
    let mut out: Vec<OrgSeverity> = by_org
        .into_iter()
        .map(|(organization, counts)| OrgSeverity {
            organization: organization.to_string(),
            counts,
        })
        .collect();
    out.sort_by(|a, b| cmp_ci(&a.organization, &b.organization));
    out
}

/// Fixed labels for the pending-patch age histogram, oldest bucket last, with the
/// undated bucket after it.
///
/// "Unknown" is its own bucket rather than being folded into `181+ days`. Undated
/// pending patches are lumped with genuinely ancient ones only if you assume the
/// worst, and the resulting bar is both the tallest and the most alarming — while
/// actually meaning "we have no timestamp", which is a data-quality signal, not a
/// backlog. Keeping it separate lets the chart tell the operator which one they are
/// looking at.
const AGE_BUCKET_LABELS: [&str; 6] = [
    "0-30 days",
    "31-60 days",
    "61-90 days",
    "91-180 days",
    // 181, not 180: a patch exactly 180 days old is in the bucket above, and a label
    // pair reading "91-180" / "180+" claimed it for both.
    "181+ days",
    "Unknown",
];

/// Index of the undated bucket in [`AGE_BUCKET_LABELS`].
const AGE_BUCKET_UNKNOWN: usize = 5;

/// Builds the pending-patch age histogram from how long NinjaOne has been reporting
/// each pending patch (see [`Patch::collected_timestamp`] — the API exposes no
/// release date, so this measures detection age, not time-since-publication).
///
/// Over the same population as every other fleet-health rollup ([`rollup_device`]),
/// which is why it needs the device inventory at all: it used to take only the
/// patches, so it was the one rollup that structurally *could not* apply the
/// exclusion the others do.
pub fn build_age_buckets(
    current_patches: &[&Patch],
    devices_by_id: &HashMap<i64, &Device>,
    now: DateTime<Utc>,
) -> Vec<AgeBucket> {
    let mut counts = [0usize; 6];
    for p in current_patches {
        if !is_pending(p.status.as_deref()) || rollup_device(devices_by_id, p.device_id).is_none() {
            continue;
        }
        let idx = match p.first_seen_at() {
            None => AGE_BUCKET_UNKNOWN,
            Some(seen) => match (now - seen).num_days().max(0) {
                0..=30 => 0,
                31..=60 => 1,
                61..=90 => 2,
                91..=180 => 3,
                _ => 4,
            },
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

/// How many stuck-approval devices ride on the IPC summary. The cached result keeps
/// every one of them for the workbook; the in-app card shows the oldest this many
/// and says how many more there are.
pub const STUCK_DEVICES_SUMMARY_CAP: usize = 200;

/// The approval workflow across the rollup population: the fleet totals of the two
/// [`ComplianceBucket`] approval columns, and the devices whose **approved** patches
/// have sat uninstalled for longer than `stuck_after_days`.
///
/// Approved-and-not-installed past the SLA is the one backlog no operator decision
/// is holding up — the approval has been given and the agent has not acted on it —
/// so it points at agent trouble (offline windows, a broken patch engine, a
/// maintenance policy that never opens) rather than at a patching backlog.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalBacklog {
    /// Fleet total of [`ComplianceBucket::awaiting_approval`].
    pub awaiting_approval: usize,
    /// Fleet total of [`ComplianceBucket::approved_not_installed`].
    pub approved_not_installed: usize,
    /// The age threshold (days since first seen) `stuck_devices` was built with —
    /// the SLA window, which is the existing "this has taken too long" knob.
    pub stuck_after_days: i64,
    /// Every stuck approved patch, across all stuck devices (not only the listed ones).
    pub stuck_patches: usize,
    /// How many devices have at least one stuck approved patch. Can exceed
    /// `stuck_devices.len()` on the IPC summary, which is capped.
    pub stuck_devices_total: usize,
    /// Per device, oldest first seen first; undated patches sort their device last.
    pub stuck_devices: Vec<StuckDevice>,
}

/// One device whose approved patches are not installing. See [`ApprovalBacklog`].
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StuckDevice {
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    /// Approved, uninstalled patches on this device first seen past the threshold.
    pub patches: usize,
    /// When NinjaOne first reported the oldest of them (detection time, not a
    /// release date). `None` when every one is undated.
    pub oldest_first_seen: Option<String>,
    pub oldest_first_seen_ts: Option<i64>,
}

impl StuckDevice {
    /// The stuck-approvals table columns, shared by the workbook and the report.
    pub const COLUMNS: [TableColumn<StuckDevice>; 4] = [
        ("Device", |d| TableCell::text(&d.device_name)),
        ("Organization", |d| TableCell::text(&d.organization)),
        ("Approved, Not Installed", |d| TableCell::Count(d.patches)),
        ("Oldest First Seen", |d| {
            TableCell::opt_text(d.oldest_first_seen.as_deref())
        }),
    ];
}

impl ApprovalBacklog {
    /// A copy carrying at most `cap` stuck devices, for the IPC summary. Every
    /// total is kept, so the frontend can say how many it is not showing.
    pub fn capped(&self, cap: usize) -> Self {
        Self {
            stuck_devices: self.stuck_devices.iter().take(cap).cloned().collect(),
            ..self.clone()
        }
    }
}

/// Builds the [`ApprovalBacklog`] from the unnarrowed current feed, over the same
/// [`rollup_device`] population as the compliance rollups — so its totals equal the
/// sums of the compliance table's two approval columns.
///
/// Stuck means `APPROVED` (the vendor status — see `approval_state`) and first seen
/// more than `stuck_after_days` ago. An undated approved patch counts as stuck, the
/// same rule the SLA-aged column applies: it cannot be shown to be recent.
pub fn build_approval_backlog(
    current_patches: &[&Patch],
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
    stuck_after_days: i64,
    now: DateTime<Utc>,
) -> ApprovalBacklog {
    struct Acc<'a> {
        device: &'a Device,
        patches: usize,
        oldest: Option<DateTime<Utc>>,
    }
    let cutoff = now - chrono::Duration::days(stuck_after_days);
    let mut out = ApprovalBacklog {
        stuck_after_days,
        ..ApprovalBacklog::default()
    };
    let mut stuck: HashMap<i64, Acc<'_>> = HashMap::new();
    for p in current_patches {
        let Some(device) = rollup_device(devices_by_id, p.device_id) else {
            continue;
        };
        match approval_state(p.status.as_deref()) {
            Some(ApprovalState::Awaiting) => out.awaiting_approval += 1,
            Some(ApprovalState::Approved) => {
                out.approved_not_installed += 1;
                let seen = p.first_seen_at();
                if seen.is_none_or(|t| t < cutoff) {
                    out.stuck_patches += 1;
                    let acc = stuck.entry(device.id).or_insert(Acc {
                        device,
                        patches: 0,
                        oldest: None,
                    });
                    acc.patches += 1;
                    if let Some(t) = seen
                        && acc.oldest.is_none_or(|o| t < o)
                    {
                        acc.oldest = Some(t);
                    }
                }
            }
            None => {}
        }
    }
    let mut devices: Vec<StuckDevice> = stuck
        .into_values()
        .map(|a| StuckDevice {
            device_id: a.device.id,
            device_name: a.device.label().to_string(),
            organization: maps.org_name(a.device.organization_id),
            patches: a.patches,
            oldest_first_seen: fmt_dt(a.oldest),
            oldest_first_seen_ts: a.oldest.map(|t| t.timestamp()),
        })
        .collect();
    // Oldest first — the longest-stuck agent is the one to look at — with undated
    // devices after every dated one, then the most stuck patches, then the name so
    // two identical runs list identically (the map's order is random per process).
    devices.sort_by(|a, b| {
        (a.oldest_first_seen_ts.is_none(), a.oldest_first_seen_ts)
            .cmp(&(b.oldest_first_seen_ts.is_none(), b.oldest_first_seen_ts))
            .then_with(|| b.patches.cmp(&a.patches))
            .then_with(|| cmp_ci(&a.device_name, &b.device_name))
            .then_with(|| a.device_id.cmp(&b.device_id))
    });
    out.stuck_devices_total = devices.len();
    out.stuck_devices = devices;
    out
}
