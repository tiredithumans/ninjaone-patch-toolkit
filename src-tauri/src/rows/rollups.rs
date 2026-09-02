//! Compact fleet-wide aggregates that ride on the summary rather than the rows:
//! install failures by patch, severity by organization, and pending-patch age
//! buckets. All three take the unnarrowed current feed and the same
//! `rollup_device` population compliance uses.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::model::{Device, Patch, PatchRow, Severity};

use super::compliance::rollup_device;
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
    /// Every affected device name, so the table and Excel/HTML export carry the
    /// complete list (not a truncated sample).
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
        ("Latest Failure", |f| {
            TableCell::opt_text(f.latest_failure.as_deref())
        }),
        ("Devices", |f| {
            // `Vec<Arc<str>>` has no `join`; the rendering is unchanged.
            TableCell::Text(
                f.device_names
                    .iter()
                    .map(|n| n.as_ref())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
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
    pub const BANDS: [SeverityBand; 8] = [
        ("Critical", |c| c.critical),
        ("Important", |c| c.important),
        ("Security", |c| c.security),
        ("Moderate", |c| c.moderate),
        ("Recommended", |c| c.recommended),
        ("Low", |c| c.low),
        ("Optional", |c| c.optional),
        ("Unknown", |c| c.unknown),
    ];

    /// Total across every band. Derived from [`BANDS`](Self::BANDS) so it can never
    /// sum a different set than the charts draw.
    pub fn total(&self) -> usize {
        Self::BANDS.iter().map(|(_, get)| get(self)).sum()
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

/// One bucket of the pending-patch age histogram (by release age).
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
    !matches!(status, Some("REJECTED") | Some("INSTALLED"))
}

/// Groups the FAILED detail rows by patch (`patch_type` + `kb` + `name`), counting
/// the distinct devices each failed on, the most recent failure, and the full list
/// of affected device names. Sorted by affected-device count then severity, desc.
pub fn build_failures(rows: &[PatchRow]) -> Vec<FailureGroup> {
    struct Acc {
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
        if &*r.status != "FAILED" {
            continue;
        }
        let acc = groups
            .entry((r.patch_type, r.kb.clone(), r.name.clone()))
            .or_insert_with(|| Acc {
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
        // that device, so the name list has no duplicates.
        if acc.devices.insert(r.device_id) {
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
        let counts = by_org.entry(org).or_default();
        match p.severity_enum() {
            Severity::Critical => counts.critical += 1,
            Severity::Important => counts.important += 1,
            Severity::Security => counts.security += 1,
            Severity::Moderate => counts.moderate += 1,
            Severity::Recommended => counts.recommended += 1,
            Severity::Low => counts.low += 1,
            Severity::Optional => counts.optional += 1,
            Severity::Unknown => counts.unknown += 1,
        }
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
/// "Unknown" is its own bucket rather than being folded into `180+ days`. Undated
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
    "180+ days",
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
