//! Joins device inventory with patch records to produce the flat per-server patch
//! rows the UI lists and the Excel exporter writes, plus the device rollups that
//! drive the reboot and compliance views.
//!
//! Adapted from `ninjaone-patch-dashboard`'s `snapshot.rs` device↔patch join.

use std::borrow::Cow;
use std::cmp::{Ordering, Reverse};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::filter::{FilterParams, PreparedFilter};
use crate::model::{Device, Location, Organization, Patch, PatchRow, PatchStatus, Role, Severity};

/// Placeholder for a name the join could not resolve — an orphan device, a device
/// reporting no OS, or a patch whose organization is not in the lookups.
const UNKNOWN_LABEL: &str = "(unknown)";

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
        self.org_name_str(id).to_string()
    }

    /// Borrowing form of [`org_name`](Self::org_name), for per-patch loops.
    fn org_name_str(&self, id: Option<i64>) -> &str {
        id.and_then(|i| self.orgs.get(&i))
            .map(String::as_str)
            .unwrap_or(UNKNOWN_LABEL)
    }

    fn location_name(&self, id: Option<i64>) -> Option<String> {
        id.and_then(|i| self.locations.get(&i)).cloned()
    }

    fn role_name(&self, id: Option<i64>) -> Option<String> {
        id.and_then(|i| self.roles.get(&i)).cloned()
    }

    /// Borrowing forms of the two above, for the per-device label resolution — the
    /// interner takes `&str` and owns the copy it keeps, so cloning here would be a
    /// `String` allocated only to be thrown away.
    fn location_name_str(&self, id: Option<i64>) -> Option<&str> {
        id.and_then(|i| self.locations.get(&i)).map(String::as_str)
    }

    fn role_name_str(&self, id: Option<i64>) -> Option<&str> {
        id.and_then(|i| self.roles.get(&i)).map(String::as_str)
    }
}

/// One slice of fetched patches tagged with its family and (for installs) a status
/// to apply when the record omits one.
pub struct PatchSource<'a> {
    pub patches: &'a [&'a Patch],
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

/// Hands out one shared [`Arc<str>`] per distinct string.
///
/// Nearly everything a [`PatchRow`] carries is drawn from a small vocabulary
/// repeated across the whole row set: an organization name recurs once per row in
/// that org, a device's name and OS once per patch on it, and a patch title once per
/// device missing it (a single Chrome update covers the fleet). Those were an owned
/// `String` per row, so the cached result — which is the app's largest live
/// allocation and is held for the lifetime of the query — stored hundreds of
/// thousands of copies of a few thousand distinct strings.
///
/// The set is keyed by the `Arc` itself, which borrows as `str`, so a hit costs a
/// hash and a refcount bump and no allocation at all.
#[derive(Default)]
struct Interner(HashSet<Arc<str>>);

impl Interner {
    fn intern(&mut self, s: &str) -> Arc<str> {
        if let Some(existing) = self.0.get(s) {
            return Arc::clone(existing);
        }
        let shared: Arc<str> = Arc::from(s);
        self.0.insert(Arc::clone(&shared));
        shared
    }

    fn intern_opt(&mut self, s: Option<&str>) -> Option<Arc<str>> {
        s.map(|s| self.intern(s))
    }
}

/// The device-derived half of a row, resolved once per device instead of once per
/// patch.
///
/// The join looks these up through `LookupMaps` and formats them per row; a device
/// with 300 pending patches did that 300 times for an answer that cannot change.
struct DeviceLabels {
    device_name: Arc<str>,
    organization: Arc<str>,
    location: Option<Arc<str>>,
    device_role: Option<Arc<str>>,
    os_name: Option<Arc<str>>,
    node_class: Option<Arc<str>>,
    needs_reboot: bool,
    offline: bool,
}

impl DeviceLabels {
    fn resolve(device: Option<&Device>, maps: &LookupMaps, pool: &mut Interner) -> Self {
        let Some(d) = device else {
            // An orphan patch — its device is not in the (possibly scoped) inventory.
            return Self {
                device_name: pool.intern(UNKNOWN_LABEL),
                organization: pool.intern(UNKNOWN_LABEL),
                location: None,
                device_role: None,
                os_name: None,
                node_class: None,
                needs_reboot: false,
                offline: false,
            };
        };
        Self {
            device_name: pool.intern(d.label()),
            organization: pool.intern(maps.org_name_str(d.organization_id)),
            location: pool.intern_opt(maps.location_name_str(d.location_id)),
            device_role: pool.intern_opt(maps.role_name_str(d.node_role_id)),
            os_name: pool.intern_opt(d.os_name_str()),
            node_class: pool.intern_opt(d.node_class.as_deref()),
            needs_reboot: d.needs_reboot(),
            offline: d.is_offline(),
        }
    }
}

/// Maps a raw NinjaOne patch status to the operator-facing label. NinjaOne uses
/// `MANUAL` for patches pending approval; show that as `PENDING` so the table
/// matches the Status filter (and NinjaOne's own UI, which labels them "Pending").
fn display_status(raw: Option<&str>) -> &str {
    match raw {
        Some("MANUAL") => "PENDING",
        Some(other) => other,
        // A record carrying no status of its own and no source-level override.
        None => "UNKNOWN",
    }
}

/// Builds detail rows from the given patch sources, resolving device/org/location/
/// role/OS names and applying the client-side OS-name and free-text filters.
pub fn build_rows(
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
    sources: &[PatchSource<'_>],
    prepared: &PreparedFilter<'_>,
) -> Vec<PatchRow> {
    let mut rows = Vec::new();
    let scope_active = prepared.has_scope();
    // One shared copy of each distinct row string, and one resolved label bundle per
    // device rather than per patch. Both are scoped to this join: the `Arc`s they
    // hand out live on in the rows, but the lookup structures are dropped here.
    let mut pool = Interner::default();
    let mut labels: HashMap<Option<i64>, DeviceLabels> = HashMap::new();
    // Reused across every row; see `Patch::write_display_name`.
    let mut name_buf = String::new();
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
            // The identity scope is enforced **here**, against the already-scoped
            // `devices_by_id`, for every source — not just the node-class facet it
            // originally covered.
            //
            // The cached current-patch feeds arrive pre-scoped (`assemble_result`
            // filters them), but the install-history rows arrive straight from the
            // API, scoped only by the `df` the server chose to honor. NinjaOne's
            // `/queries/*` endpoints ignore `class` outright, and a mistyped or
            // unsupported clause is silently dropped rather than rejected — so a
            // narrowed query could return, and display, rows from devices the
            // operator had scoped out. Requiring an in-scope device makes the
            // client-side scope authoritative and the `df` purely an optimization.
            //
            // With no identity scope at all, orphan patches (no matching device in
            // inventory) are still kept, as they always were: there is no scope for
            // them to fall outside of.
            if scope_active && device.is_none() {
                continue;
            }
            // Borrowed for the filter check; the owned copy is taken below, only
            // for rows that survive. Allocating here cost one String per patch
            // examined rather than per patch kept — and on a whole-fleet
            // third-party feed the filters discard the large majority.
            let os_name_ref = device.and_then(Device::os_name_str);

            if !prepared.os_name_allowed(os_name_ref) {
                continue;
            }
            if !prepared.search_allowed(patch.kb_number.as_deref(), patch.name.as_deref()) {
                continue;
            }

            let severity = patch.severity_enum();
            if !prepared.severity_allowed(severity) {
                continue;
            }
            let first_seen = patch.first_seen_at();
            let installed = patch.installed_at();
            if !prepared.detected_within_allowed(first_seen.map(|r| r.timestamp())) {
                continue;
            }
            let status = display_status(patch.status.as_deref().or(source.status_override));
            patch.write_display_name(&mut name_buf);

            // Resolved once per device id, then shared by every row on it.
            let device_labels = labels
                .entry(patch.device_id)
                .or_insert_with(|| DeviceLabels::resolve(device, maps, &mut pool));

            rows.push(PatchRow {
                device_id: patch.device_id.unwrap_or_default(),
                device_name: Arc::clone(&device_labels.device_name),
                organization: Arc::clone(&device_labels.organization),
                location: device_labels.location.clone(),
                device_role: device_labels.device_role.clone(),
                os_name: device_labels.os_name.clone(),
                node_class: device_labels.node_class.clone(),
                needs_reboot: device_labels.needs_reboot,
                offline: device_labels.offline,
                patch_type: source.type_label,
                kb: pool.intern_opt(patch.kb_number.as_deref()),
                name: pool.intern(&name_buf),
                severity: severity.label(),
                severity_rank: severity.rank(),
                status: pool.intern(status),
                first_seen_date: fmt_dt(first_seen),
                installed_date: fmt_dt(installed),
                // Normalised through `first_seen_at`/`installed_at` like the dates
                // beside them, NOT read raw off the patch. NinjaOne returns
                // milliseconds for these on some endpoints, and taking the raw value
                // made a row disagree with itself: it displayed the correct date
                // (which goes through `unix_to_datetime`) while sorting as a
                // year-58000 timestamp — so a millisecond-valued record always won
                // "latest failure" and the First-seen sort put it on top.
                first_seen_ts: first_seen.map(|d| d.timestamp()),
                installed_ts: installed.map(|d| d.timestamp()),
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
/// NinjaOne uses MANUAL (pending approval) and APPROVED for current patches not yet
/// installed — both count. The rank threshold deliberately excludes `Security` and
/// `Recommended`, which are NinjaOne *classifications* rather than urgency grades.
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
/// denominator rather than scored compliant and inflating the headline metric.
fn rollup_device<'a>(
    devices_by_id: &HashMap<i64, &'a Device>,
    device_id: Option<i64>,
) -> Option<&'a Device> {
    devices_by_id
        .get(&device_id?)
        .copied()
        .filter(|d| !d.is_offline())
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

/// Counts current pending/approved patches per device for compliance and the
/// reboot/summary views. Shares [`is_pending`] with every other pending rollup, so
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

/// One rendered cell of a table row. `Count`/`Number` are written as numbers by the
/// Excel exporter and right-aligned by the HTML report; `Text` is written as-is
/// (and HTML-escaped by the report).
pub enum TableCell {
    Text(String),
    Count(usize),
    Number(f64),
}

impl TableCell {
    /// A text cell from anything string-like. The row fields are a mix of `String`,
    /// `Arc<str>` and `&'static str` now that [`PatchRow`] shares its repeated
    /// values, and a renderer should not have to care which.
    pub fn text(value: impl AsRef<str>) -> Self {
        Self::Text(value.as_ref().to_string())
    }

    /// A text cell for an optional field, blank when absent — which every renderer
    /// already spelled out as `.clone().unwrap_or_default()`.
    pub fn opt_text(value: Option<impl AsRef<str>>) -> Self {
        Self::Text(value.map(|v| v.as_ref().to_string()).unwrap_or_default())
    }
}

/// One table column: its header and how to read that cell off a row.
///
/// Every table rendered from a cached [`QueryResult`] is defined as an array of
/// these, so a column's header and its value are one declaration rather than two
/// lists that agree by convention. That convention had already failed twice — the
/// HTML report dropped `Patch Type` from the failures table the workbook wrote,
/// and the reboot table's headers drifted to "Role" against the workbook's "Device
/// Role" — which is why the definitions now live here, next to the data, instead
/// of once per renderer.
pub type TableColumn<T> = (&'static str, fn(&T) -> TableCell);

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
}

/// Rounds a percentage to one decimal for display, so the workbook and the report
/// cannot disagree about precision — and never *up* to a full 100. See
/// [`format_pct`], which applies the same rule at zero decimals.
fn pct_cell(pct: f64) -> TableCell {
    let rounded = (pct * 10.0).round() / 10.0;
    TableCell::Number(if pct >= 100.0 {
        100.0
    } else {
        rounded.min(99.9)
    })
}

/// Formats a compliance percentage at zero decimals **without ever rounding up to
/// 100%**.
///
/// A compliance report that prints "100%" for a fleet that is not fully compliant is
/// the one rounding error in this app that can send someone home: plain `{:.0}%`
/// renders anything from 99.5% up as `100%`, so 199 of 200 devices patched read as
/// "done". Values below 100 are capped at 99%, which understates by less than a
/// point and never claims a clean fleet that isn't. Exactly 100.0 still prints
/// `100%`.
///
/// Mirrored in `web-rs/src/app/util.rs::format_pct` for the in-app tables and charts;
/// the two crates share no code, so the rule is written twice on purpose.
pub fn format_pct(pct: f64) -> String {
    let shown = if pct >= 100.0 {
        100.0
    } else {
        pct.round().min(99.0)
    };
    format!("{shown:.0}%")
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
fn is_pending(status: Option<&str>) -> bool {
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
pub fn compliance_scope_note(devices_offline: usize, families: PatchFamilies) -> String {
    let excluded = match devices_offline {
        0 => String::new(),
        1 => " (1 offline device excluded)".to_string(),
        n => format!(" ({n} offline devices excluded)"),
    };
    let families = if families.is_complete() {
        String::new()
    } else {
        format!(", and counts {}", families.label())
    };
    format!("Compliance covers online devices only{excluded}{families}.")
}

/// The facets that produced a result, resolved to the names the operator chose them
/// by — the provenance block both exports print.
///
/// Derived backend-side from the `QueryPlan` the fetch actually ran under, **not**
/// taken from the request or from the frontend's `AppliedFilters` chip row. Those
/// describe what was *selected*; this describes what the query *did*, which is the
/// distinction that matters once a workbook has been emailed to someone who never
/// saw the app. It is the same rule the write path follows for the same reason: the
/// backend re-derives rather than trusting a caller's description of its own request.
///
/// Carried on [`QueryResult`] **only**, deliberately not on [`QuerySummary`]: the
/// frontend already has `AppliedFilters` (a Run-time snapshot with its own chip
/// rendering), so shipping a second copy over IPC would add a wire field with no
/// reader and a second thing to keep in step.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryScope {
    /// `(facet, value)` in display order. Never empty — the patch type and the
    /// status selection always apply.
    pub facets: Vec<(&'static str, String)>,
}

/// Renders one id facet as a comma-separated name list.
///
/// An id the lookups can't resolve prints as `id 42` rather than the rows'
/// `(unknown)`: two unresolved ids would otherwise render as `(unknown), (unknown)`,
/// which says neither how many were selected nor which.
fn id_facet(ids: &[i64], names: &HashMap<i64, String>) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    let mut out: Vec<String> = ids
        .iter()
        .map(|id| names.get(id).cloned().unwrap_or_else(|| format!("id {id}")))
        .collect();
    out.sort_by(|a, b| cmp_ci(a, b));
    Some(out.join(", "))
}

/// Builds the provenance block from the plan a query ran under.
///
/// `install_history_after` is `Some` only when the status selection actually reached
/// the `*-patch-installs` endpoints; printing a lookback window on a Pending-only
/// query would describe a fetch that never happened.
pub fn build_query_scope(
    filter: &FilterParams,
    maps: &LookupMaps,
    families: PatchFamilies,
    statuses: &[PatchStatus],
    install_history_after: Option<i64>,
) -> QueryScope {
    let mut facets: Vec<(&'static str, String)> = Vec::new();

    let organizations = id_facet(&filter.organization_ids, &maps.orgs);
    let locations = id_facet(&filter.location_ids, &maps.locations);
    let roles = id_facet(&filter.role_ids, &maps.roles);
    let os_types = (!filter.node_classes.is_empty()).then(|| filter.node_classes.join(", "));
    let severities = (!filter.severities.is_empty()).then(|| filter.severities.join(", "));
    // Absolute bounds, because a report is read long after the day it was run:
    // "the last 30 days" silently re-anchors to whenever the reader happens to
    // look, while a timestamp does not. The relative window rides along in
    // parentheses when that is what the operator picked, so the line still matches
    // the control they used.
    // `from_timestamp` rather than `model::unix_to_datetime`: these three bounds are
    // composed backend-side as Unix *seconds* (`QueryPlan::build`), never read off a
    // NinjaOne record, so the millisecond normalization that helper exists for
    // cannot apply here.
    let stamp = |ts: i64| {
        DateTime::<Utc>::from_timestamp(ts, 0).map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
    };
    let detected_after =
        filter
            .detected_after
            .and_then(stamp)
            .map(|when| match filter.detected_within_days {
                Some(days) => format!("{when} (last {days} days)"),
                None => when,
            });
    let detected_before = filter.detected_before.and_then(stamp);

    // Stated rather than left to inference: on a printed artifact the absence of
    // narrowing lines is indistinguishable from a renderer that dropped them.
    let narrowed = [
        &organizations,
        &locations,
        &roles,
        &os_types,
        &severities,
        &detected_after,
        &detected_before,
        &filter.os_name_contains,
        &filter.search,
    ]
    .iter()
    .any(|f| f.is_some());
    if !narrowed {
        facets.push((
            "Scope",
            "Whole fleet \u{2014} no device or patch filters applied".to_string(),
        ));
    }

    for (label, value) in [
        ("Organizations", organizations),
        ("Locations", locations),
        ("Device roles", roles),
        ("OS type", os_types),
        ("OS name contains", filter.os_name_contains.clone()),
    ] {
        if let Some(value) = value {
            facets.push((label, value));
        }
    }

    facets.push(("Patch type", families.label().to_string()));
    facets.push((
        "Status",
        statuses
            .iter()
            .map(|s| s.label())
            .collect::<Vec<_>>()
            .join(", "),
    ));

    for (label, value) in [
        ("Severity", severities),
        ("Search", filter.search.clone()),
        ("First seen after", detected_after),
        ("First seen before", detected_before),
        (
            "Install history since",
            install_history_after.and_then(stamp),
        ),
    ] {
        if let Some(value) = value {
            facets.push((label, value));
        }
    }

    QueryScope { facets }
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
    /// Compliance grouped by OS name (the Compliance tab's "Compliance by OS").
    pub compliance_by_os: Vec<OsCompliance>,
    /// FAILED-install rollup (empty unless the FAILED status was queried).
    pub failures: Vec<FailureGroup>,
    /// Per-org pending-patch severity breakdown for the dashboard.
    pub severity_by_org: Vec<OrgSeverity>,
    /// Pending-patch age histogram for the dashboard.
    pub age_buckets: Vec<AgeBucket>,
    pub devices_total: usize,
    /// How many of `devices_total` are offline.
    ///
    /// The compliance rollups deliberately exclude offline devices from both halves
    /// of every bucket (they report no current patch records, so a zero pending count
    /// says nothing about them) — which means the `Devices` column of the compliance
    /// table does **not** sum to `devices_total`, and used to have nothing on screen
    /// explaining the gap. Carried so the UI, the workbook and the HTML report can
    /// state the excluded population instead of leaving two different device counts
    /// side by side.
    pub devices_offline: usize,
    /// Which patch families the fleet-health rollups actually cover, taken from the
    /// query's patch-type facet.
    ///
    /// The compliance/severity/age rollups are computed from the *current* patch
    /// feeds, and only the families the query asked for are fetched at all — a
    /// whole-fleet third-party feed is the largest fetch in the app, so an OS-only
    /// query does not page it. That makes "compliant" mean "no pending OS patches"
    /// on such a query, which is a defensible reading but not one the operator can
    /// infer from a bare percentage. Reported so every surface can name its scope.
    pub patch_families: PatchFamilies,
    /// The facets this result was computed under, for the exports' provenance block.
    /// `QueryResult`-only on purpose — see [`QueryScope`].
    pub scope: QueryScope,
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
    /// Compliance grouped by OS name (the Compliance tab's "Compliance by OS").
    pub compliance_by_os: Vec<OsCompliance>,
    /// FAILED-install rollup — small (one entry per failing patch), so it ships
    /// whole rather than paged like the detail rows.
    pub failures: Vec<FailureGroup>,
    /// Per-org pending-patch severity breakdown for the dashboard charts.
    pub severity_by_org: Vec<OrgSeverity>,
    /// Pending-patch age histogram for the dashboard charts.
    pub age_buckets: Vec<AgeBucket>,
    pub devices_total: usize,
    /// How many of `devices_total` are offline.
    ///
    /// The compliance rollups deliberately exclude offline devices from both halves
    /// of every bucket (they report no current patch records, so a zero pending count
    /// says nothing about them) — which means the `Devices` column of the compliance
    /// table does **not** sum to `devices_total`, and used to have nothing on screen
    /// explaining the gap. Carried so the UI, the workbook and the HTML report can
    /// state the excluded population instead of leaving two different device counts
    /// side by side.
    pub devices_offline: usize,
    /// Which patch families the fleet-health rollups actually cover, taken from the
    /// query's patch-type facet.
    ///
    /// The compliance/severity/age rollups are computed from the *current* patch
    /// feeds, and only the families the query asked for are fetched at all — a
    /// whole-fleet third-party feed is the largest fetch in the app, so an OS-only
    /// query does not page it. That makes "compliant" mean "no pending OS patches"
    /// on such a query, which is a defensible reading but not one the operator can
    /// infer from a bare percentage. Reported so every surface can name its scope.
    pub patch_families: PatchFamilies,
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
            compliance_by_os: result.compliance_by_os.clone(),
            failures: result.failures.clone(),
            severity_by_org: result.severity_by_org.clone(),
            age_buckets: result.age_buckets.clone(),
            devices_total: result.devices_total,
            devices_offline: result.devices_offline,
            patch_families: result.patch_families,
            generated_at: result.generated_at.clone(),
            data_fetched_at: result.data_fetched_at.clone(),
        }
    }
}

/// Sort key for the paged detail rows (`get_patch_rows`). Deserialized from the
/// frontend's camelCase IPC args; mirrored in `web-rs/src/types.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
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

/// `PartialEq` so the cache can tell whether its memoized order still answers the
/// sort being asked for — the same check `with_grouped_result` makes on [`GroupBy`].
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowSort {
    pub key: RowSortKey,
    pub desc: bool,
}

/// Which key the Patches view groups its rows by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum GroupBy {
    Device,
    Patch,
}

/// Separator joining the fields of a composite group key. A unit separator can't
/// occur in a device or patch name, so one group's key can never collide with or
/// forge another's.
const GROUP_KEY_SEP: char = '\u{1f}';

/// The stable identity the frontend echoes back to fetch a group's members and to
/// key its expand state. Keyed on the same tuple the group is built from, so it
/// round-trips without the backend holding per-request state.
#[cfg(test)]
pub fn group_key(row: &PatchRow, group_by: GroupBy) -> String {
    let mut buf = String::new();
    write_group_key(row, group_by, &mut buf);
    buf
}

/// The same key, into a caller-owned buffer which it clears first. This is the form
/// production uses; the owned `group_key` above remains as the readable statement of
/// the encoding, which the tests hold `GroupKeyMatcher` against.
///
/// [`build_groups`] needs a key for every row purely to look one up, and almost all
/// of those lookups hit an existing group. A reused buffer turns that into zero
/// allocations per row; only a row that opens a *new* group pays for a `String`.
fn write_group_key(row: &PatchRow, group_by: GroupBy, buf: &mut String) {
    use std::fmt::Write as _;
    buf.clear();
    match group_by {
        GroupBy::Device => {
            let _ = write!(buf, "{}", row.device_id);
        }
        GroupBy::Patch => {
            let _ = write!(
                buf,
                "{}{GROUP_KEY_SEP}{}{GROUP_KEY_SEP}{}",
                row.patch_type,
                row.kb.as_deref().unwrap_or(""),
                row.name
            );
        }
    }
}

/// One collapsed group header. Members are **not** carried: a patch group can span
/// the whole fleet (a single Chrome update covers every device), so members are
/// paged separately via [`group_member_page`] when the operator expands it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PatchGroup {
    pub key: String,
    pub label: Arc<str>,
    /// Organization for a device group; KB for a patch group (blank when absent —
    /// third-party patches carry no KB).
    pub sublabel: Option<Arc<str>>,
    pub rows: usize,
    /// Distinct devices in the group: always 1 for a device group, and the
    /// affected-device count for a patch group.
    pub devices: usize,
    /// Highest severity among the members, so a collapsed group still shows how
    /// urgent its worst patch is.
    pub severity: &'static str,
    pub severity_rank: u8,
    /// Device groups only — the id actions dispatch against, and its state.
    pub device_id: Option<i64>,
    pub offline: bool,
    pub needs_reboot: bool,
}

/// Builds every group over the cached rows, ordered most-urgent-first.
///
/// Device groups keep the canonical severity → org → device order the flat view
/// uses; patch groups lead with blast radius (affected devices) then severity,
/// matching [`build_failures`], because "this update is missing on 212 machines"
/// is the thing worth seeing first.
pub fn build_groups(rows: &[PatchRow], group_by: GroupBy) -> Vec<PatchGroup> {
    struct Acc {
        /// Insertion order, which the stable display sorts fall back to on a tie.
        seq: usize,
        label: Arc<str>,
        sublabel: Option<Arc<str>>,
        rows: usize,
        devices: HashSet<i64>,
        severity: &'static str,
        severity_rank: u8,
        device_id: Option<i64>,
        offline: bool,
        needs_reboot: bool,
    }
    let mut groups: HashMap<String, Acc> = HashMap::new();
    // One reusable key buffer for the whole pass — see `write_group_key`. Grouping
    // used to cost three `String`s per row (the key, and a clone each for the map
    // entry and the insertion-order list); it now costs one per *group*.
    let mut key_buf = String::new();
    for r in rows {
        write_group_key(r, group_by, &mut key_buf);
        // Insertion order is load-bearing: the display sorts below are stable, so
        // tied groups fall back to it, and it follows the canonical row order.
        // Carrying it on the accumulator replaces the parallel `order` vec.
        let seq = groups.len();
        let acc = match groups.get_mut(&key_buf) {
            Some(acc) => acc,
            None => groups
                .entry(key_buf.clone())
                .or_insert_with(|| match group_by {
                    GroupBy::Device => Acc {
                        seq,
                        label: r.device_name.clone(),
                        sublabel: Some(r.organization.clone()),
                        rows: 0,
                        devices: HashSet::new(),
                        severity: r.severity,
                        severity_rank: r.severity_rank,
                        device_id: Some(r.device_id),
                        offline: r.offline,
                        needs_reboot: r.needs_reboot,
                    },
                    GroupBy::Patch => Acc {
                        seq,
                        label: r.name.clone(),
                        sublabel: r.kb.clone().filter(|k| !k.is_empty()),
                        rows: 0,
                        devices: HashSet::new(),
                        severity: r.severity,
                        severity_rank: r.severity_rank,
                        device_id: None,
                        offline: false,
                        needs_reboot: false,
                    },
                }),
        };
        acc.rows += 1;
        acc.devices.insert(r.device_id);
        // Records for the same group can disagree; surface the worst.
        if r.severity_rank > acc.severity_rank {
            acc.severity_rank = r.severity_rank;
            acc.severity = r.severity;
        }
    }

    let mut accumulated: Vec<(String, Acc)> = groups.into_iter().collect();
    accumulated.sort_unstable_by_key(|(_, a)| a.seq);
    let mut out: Vec<PatchGroup> = accumulated
        .into_iter()
        .map(|(key, a)| PatchGroup {
            key,
            label: a.label,
            sublabel: a.sublabel,
            rows: a.rows,
            devices: a.devices.len(),
            severity: a.severity,
            severity_rank: a.severity_rank,
            device_id: a.device_id,
            offline: a.offline,
            needs_reboot: a.needs_reboot,
        })
        .collect();

    match group_by {
        GroupBy::Device => out.sort_by_cached_key(|g| {
            (
                Reverse(g.severity_rank),
                g.sublabel.as_deref().unwrap_or_default().to_lowercase(),
                g.label.to_lowercase(),
            )
        }),
        GroupBy::Patch => {
            out.sort_by_cached_key(|g| (Reverse(g.devices), Reverse(g.severity_rank)))
        }
    }
    out
}

/// One page of group headers plus the total, so the frontend can page groups the
/// same way it pages flat rows.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupPage {
    pub groups: Vec<PatchGroup>,
    pub total: usize,
}

/// Slices an already-built grouping into one page.
///
/// Split from the building so the caller can memoize the expensive half: this used
/// to be `group_page`, which rebuilt the whole grouping on every paging request. The
/// arithmetic stays here, where it is unit-testable, rather than inline in the
/// command.
pub fn slice_groups(all: &[PatchGroup], offset: usize, limit: usize) -> GroupPage {
    GroupPage {
        total: all.len(),
        groups: all.iter().skip(offset).take(limit).cloned().collect(),
    }
}

/// One page of a single group's member rows, in the cache's canonical order.
/// Filtering by key rather than storing members on the group keeps a fleet-wide
/// patch group (one entry per device) off the wire until it's actually expanded.
pub fn group_member_page(
    rows: &[PatchRow],
    group_by: GroupBy,
    key: &str,
    offset: usize,
    limit: usize,
) -> Vec<PatchRow> {
    // The key is parsed once and each row compared field-by-field. Building
    // `group_key(r, group_by)` per row allocated a `format!`ed String for every row
    // in the cache on every expand and every page of a group — a whole-fleet
    // allocation to serve twenty rows.
    let matcher = GroupKeyMatcher::new(group_by, key);
    rows.iter()
        .filter(|r| matcher.matches(r))
        .skip(offset)
        .take(limit)
        .cloned()
        .collect()
}

/// A parsed [`group_key`], so membership is an equality check rather than a fresh
/// `String` per row. Mirrors `group_key`'s encoding exactly — a change to one is a
/// change to the other, which `group_key_and_matcher_agree` pins.
enum GroupKeyMatcher<'a> {
    Device(Option<i64>),
    Patch {
        patch_type: &'a str,
        kb: &'a str,
        name: &'a str,
    },
    /// A key that does not parse matches nothing, exactly as a non-equal string did.
    None,
}

impl<'a> GroupKeyMatcher<'a> {
    fn new(group_by: GroupBy, key: &'a str) -> Self {
        match group_by {
            GroupBy::Device => Self::Device(key.parse().ok()),
            GroupBy::Patch => {
                let mut parts = key.split(GROUP_KEY_SEP);
                match (parts.next(), parts.next(), parts.next(), parts.next()) {
                    (Some(patch_type), Some(kb), Some(name), None) => Self::Patch {
                        patch_type,
                        kb,
                        name,
                    },
                    _ => Self::None,
                }
            }
        }
    }

    fn matches(&self, row: &PatchRow) -> bool {
        match self {
            Self::Device(Some(id)) => row.device_id == *id,
            Self::Device(None) | Self::None => false,
            Self::Patch {
                patch_type,
                kb,
                name,
            } => {
                row.patch_type == *patch_type
                    && row.kb.as_deref().unwrap_or("") == *kb
                    && &*row.name == *name
            }
        }
    }
}

/// The row order `sort` implies, as a permutation of indices into `rows`.
///
/// Split from the slicing for the same reason [`build_groups`] is split from
/// [`slice_groups`]: this is the expensive half and the caller memoizes it. Paging
/// through a sorted view used to re-sort the entire cached row set on **every page
/// request** — a full `O(n log n)` string comparison sweep per click, under the lock
/// the export also takes — while the identical problem on the grouping side had
/// already been fixed. The permutation is `u32` rather than `&PatchRow` so the memo
/// costs 4 bytes a row instead of a pointer plus a second copy of the set.
///
/// The cached rows themselves are never reordered; their canonical order is
/// load-bearing for the export and for the summary's inline first page.
pub fn sort_order(rows: &[PatchRow], sort: RowSort) -> Vec<u32> {
    let mut order: Vec<u32> = (0..rows.len() as u32).collect();
    // Stable sort: rows that tie keep the canonical cache order.
    order.sort_by(|a, b| compare_rows(&rows[*a as usize], &rows[*b as usize], sort));
    order
}

/// Serves one page of the cached detail rows, in `order` when one is supplied.
///
/// `None` reproduces the cache order exactly (the canonical severity/org/device sort
/// stamped in `run_query`), without materializing an identity permutation for a fleet
/// that never asked to be re-sorted. Only the requested page is cloned either way.
///
/// An index in `order` that is out of range is skipped rather than panicking: the
/// permutation and the rows come from the same cache entry under one lock, so this
/// is unreachable, but a page request is not worth a process abort if that ever
/// stops being true.
pub fn page_rows(
    rows: &[PatchRow],
    order: Option<&[u32]>,
    offset: usize,
    limit: usize,
) -> Vec<PatchRow> {
    match order {
        None => rows.iter().skip(offset).take(limit).cloned().collect(),
        Some(order) => order
            .iter()
            .skip(offset)
            .take(limit)
            .filter_map(|i| rows.get(*i as usize))
            .cloned()
            .collect(),
    }
}

fn compare_rows(a: &PatchRow, b: &PatchRow, sort: RowSort) -> Ordering {
    use RowSortKey::*;
    let dir = |o: Ordering| if sort.desc { o.reverse() } else { o };
    match sort.key {
        Organization => dir(cmp_ci(&a.organization, &b.organization)),
        Location => cmp_opt_last(
            a.location.as_deref(),
            b.location.as_deref(),
            sort.desc,
            |x, y| cmp_ci(x, y),
        ),
        Role => cmp_opt_last(
            a.device_role.as_deref(),
            b.device_role.as_deref(),
            sort.desc,
            |x, y| cmp_ci(x, y),
        ),
        Device => dir(cmp_ci(&a.device_name, &b.device_name)),
        Os => cmp_opt_last(
            a.os_name.as_deref(),
            b.os_name.as_deref(),
            sort.desc,
            |x, y| cmp_ci(x, y),
        ),
        PatchType => dir(a.patch_type.cmp(b.patch_type)),
        Kb => cmp_opt_last(a.kb.as_deref(), b.kb.as_deref(), sort.desc, |x, y| {
            cmp_ci(x, y)
        }),
        Name => dir(cmp_ci(&a.name, &b.name)),
        // The severity ordinal is presentation order (Critical → Unknown), so an
        // ascending sort surfaces the most urgent first, like the default view.
        Severity => dir(b.severity_rank.cmp(&a.severity_rank)),
        Status => dir(a.status.cmp(&b.status)),
        FirstSeenDate => cmp_opt_last(a.first_seen_ts, b.first_seen_ts, sort.desc, |x, y| x.cmp(y)),
        InstalledDate => cmp_opt_last(a.installed_ts, b.installed_ts, sort.desc, |x, y| x.cmp(y)),
    }
}

/// Case-insensitive (ASCII) ordering without a per-comparison allocation.
pub fn cmp_ci(a: &str, b: &str) -> Ordering {
    a.bytes()
        .map(|c| c.to_ascii_lowercase())
        .cmp(b.bytes().map(|c| c.to_ascii_lowercase()))
}

/// Missing values sort last regardless of direction, so a descending sort by
/// e.g. installed date leads with real dates rather than blanks.
fn cmp_opt_last<T, F>(a: Option<T>, b: Option<T>, desc: bool, cmp: F) -> Ordering
where
    F: Fn(&T, &T) -> Ordering,
{
    match (&a, &b) {
        (Some(x), Some(y)) => {
            let o = cmp(x, y);
            if desc { o.reverse() } else { o }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use crate::filter::FilterParams;

    /// Composes the two halves of paging the way `AppState::with_sorted_result`
    /// does — build the order once, then slice it — so these assertions still
    /// describe what a real page request produces. Keeping the composition in one
    /// helper is also what makes a divergence between `sort_order` and `page_rows`
    /// visible here rather than only in the app.
    fn sorted_page(
        rows: &[PatchRow],
        offset: usize,
        limit: usize,
        sort: Option<RowSort>,
    ) -> Vec<PatchRow> {
        let order = sort.map(|s| sort_order(rows, s));
        page_rows(rows, order.as_deref(), offset, limit)
    }

    /// Borrows an owned patch fixture into the `&[&Patch]` shape the rollups take.
    /// Production builds these by filtering the `Arc` cache; the tests own theirs.
    fn refs(patches: &[Patch]) -> Vec<&Patch> {
        patches.iter().collect()
    }

    /// Guards the one hand-maintained pairing left in the severity vocabulary: a
    /// field added to `SeverityCounts` but not to `BANDS` makes `total()` disagree
    /// with the fields, and everything derived from `BANDS` (the report's chart, its
    /// legend, its denominator) would silently drop that band.
    ///
    /// Distinct prime-ish values so a duplicated or transposed accessor is caught
    /// too, not just a missing one.
    #[test]
    fn total_severity_is_the_sum_of_its_bands() {
        let c = SeverityCounts {
            critical: 2,
            important: 3,
            security: 5,
            moderate: 7,
            recommended: 11,
            low: 13,
            optional: 17,
            unknown: 19,
        };
        assert_eq!(
            c.total(),
            2 + 3 + 5 + 7 + 11 + 13 + 17 + 19,
            "SeverityCounts::BANDS must cover every field exactly once"
        );

        // Each accessor must read a distinct field, in the declared order.
        let read: Vec<usize> = SeverityCounts::BANDS
            .iter()
            .map(|(_, get)| get(&c))
            .collect();
        assert_eq!(read, vec![2, 3, 5, 7, 11, 13, 17, 19]);

        let labels: Vec<&str> = SeverityCounts::BANDS.iter().map(|(l, _)| *l).collect();
        assert_eq!(
            labels,
            vec![
                "Critical",
                "Important",
                "Security",
                "Moderate",
                "Recommended",
                "Low",
                "Optional",
                "Unknown"
            ],
            "band order mirrors Severity::rank(), most urgent first"
        );
    }

    /// `AddAssign` is the other field-wise site; it must agree with `BANDS` too.
    #[test]
    fn severity_counts_add_assign_sums_every_band() {
        let a = SeverityCounts {
            critical: 1,
            important: 2,
            security: 3,
            moderate: 4,
            recommended: 5,
            low: 6,
            optional: 7,
            unknown: 8,
        };
        let mut sum = SeverityCounts::default();
        sum += &a;
        sum += &a;
        assert_eq!(sum.total(), a.total() * 2);
        for ((_, get), expected) in SeverityCounts::BANDS
            .iter()
            .zip([2, 4, 6, 8, 10, 12, 14, 16])
        {
            assert_eq!(get(&sum), expected);
        }
    }
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
            collected_timestamp: released_days_ago
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

    /// A row must not disagree with itself. Some NinjaOne endpoints return these
    /// `*At` fields in **milliseconds**; the displayed date goes through
    /// `unix_to_datetime` (which normalises), so writing the raw value into the sort
    /// timestamp made a millisecond-valued record render as 2026 while sorting as a
    /// year-58000 date — always winning "latest failure" and the First-seen sort.
    #[test]
    fn row_timestamps_are_normalised_like_the_dates_beside_them() {
        let seconds = 1_777_000_000_f64;
        let mut ms_patch = patch(1, "FAILED", "CRITICAL", None);
        ms_patch.collected_timestamp = Some(seconds * 1000.0);
        ms_patch.installed_timestamp = Some(seconds * 1000.0);

        let devices = [device(1, 10, "Windows Server 2022")];
        let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();
        let patches = vec![ms_patch];
        let rows = build_rows(
            &by_id,
            &maps(),
            &[PatchSource {
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default().prepare(),
        );

        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(
            r.first_seen_ts,
            Some(seconds as i64),
            "a millisecond value must be normalised, not stored raw"
        );
        assert_eq!(r.installed_ts, Some(seconds as i64));
        // And the timestamp must agree with the date rendered next to it.
        let from_ts = DateTime::<Utc>::from_timestamp(r.first_seen_ts.unwrap(), 0).unwrap();
        assert_eq!(
            r.first_seen_date,
            fmt_dt(Some(from_ts)),
            "the sort timestamp and the displayed date must describe the same instant"
        );
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &filter.prepare(),
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(&*rows[0].organization, "Contoso");
        assert_eq!(rows[0].location.as_deref(), Some("HQ"));
        assert_eq!(rows[0].device_role.as_deref(), Some("Domain Controller"));
        assert_eq!(rows[0].patch_type, "OS");
    }

    #[test]
    fn first_seen_filter_narrows_rows() {
        let d1 = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d1)]);
        let maps = maps();
        let patches = vec![
            patch(1, "PENDING", "CRITICAL", Some(2)), // released 2 days ago → kept
            patch(1, "PENDING", "CRITICAL", Some(100)), // released 100 days ago → dropped
        ];
        let cutoff = (Utc::now() - Duration::days(10)).timestamp();
        let filter = FilterParams {
            detected_after: Some(cutoff),
            ..Default::default()
        };
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &filter.prepare(),
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &filter.prepare(),
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: Some("INSTALLED"),
                status_filter: None,
            }],
            &FilterParams::default().prepare(),
        );
        assert_eq!(&*rows[0].status, "INSTALLED");
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: Some(&pending_set),
            }],
            &FilterParams::default().prepare(),
        );
        assert_eq!(rows.len(), 1, "a MANUAL patch matches the Pending filter");
        assert_eq!(&*rows[0].status, "PENDING", "MANUAL renders as PENDING");
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: Some("INSTALLED"),
                status_filter: Some(&failed_set),
            }],
            &FilterParams::default().prepare(),
        );
        assert_eq!(rows.len(), 1, "only the FAILED install record is kept");
        assert_eq!(&*rows[0].status, "FAILED");
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: Some("INSTALLED"),
                status_filter: Some(&installed_set),
            }],
            &FilterParams::default().prepare(),
        );
        assert_eq!(rows.len(), 1, "missing status falls back to the override");
        assert_eq!(&*rows[0].status, "INSTALLED");
    }

    /// `is_pending` is an exclude list. The allow list it replaced scored a device
    /// whose only patch sat in the current feed as FAILED — or under any value the
    /// crate had not anticipated — as compliant, which is the one direction this
    /// predicate must never fail in.
    #[test]
    fn a_current_feed_record_is_pending_unless_rejected_or_installed() {
        for pending in [
            Some("MANUAL"),
            Some("APPROVED"),
            Some("FAILED"),
            Some("SOMETHING_NEW"),
            None,
        ] {
            assert!(is_pending(pending), "{pending:?} must count as pending");
        }
        assert!(!is_pending(Some("REJECTED")));
        assert!(!is_pending(Some("INSTALLED")));
    }

    /// The row join has to agree with the rollups about an untyped current-feed
    /// record: `pending_counts` counts it, so the Pending selection must show it.
    /// `assemble_result` labels the current sources MANUAL for exactly this.
    #[test]
    fn an_untyped_current_record_is_a_pending_row_under_the_pending_override() {
        use crate::model::PatchStatus;
        let d = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d)]);
        let maps = maps();
        let mut untyped = patch(1, "MANUAL", "CRITICAL", Some(1));
        untyped.status = None;
        let patches = vec![untyped];
        let refs = refs(&patches);
        assert_eq!(pending_counts(&refs).get(&1), Some(&1));

        let pending_set = HashSet::from([PatchStatus::Pending.api_value()]);
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &refs,
                type_label: "OS",
                status_override: Some(PatchStatus::Pending.api_value()),
                status_filter: Some(&pending_set),
            }],
            &FilterParams::default().prepare(),
        );
        assert_eq!(rows.len(), 1, "the record the rollups count is also a row");
        assert_eq!(&*rows[0].status, "PENDING");
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
        let counts = pending_counts(&refs(&current));
        let summaries = build_device_summaries(&[&d1, &d2], &counts, &maps);
        let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
        assert_eq!(buckets.len(), 1);
        let b = &buckets[0];
        assert_eq!(b.devices_total, 2);
        assert_eq!(b.devices_compliant, 1);
        assert_eq!(b.pending_critical, 2);
        assert_eq!(b.aged_critical, 1);
        assert!((b.compliance_pct - 50.0).abs() < 1e-9);
    }

    /// The two halves of a compliance bucket must describe the *same* devices.
    ///
    /// The device half already skipped offline devices; the patch half did not, so an
    /// org whose devices were all offline produced a row reading "0 devices,
    /// 100% compliant, 40 pending Critical/Important" — a full green bar drawn from
    /// the backlog of the very devices the percentage refused to look at.
    #[test]
    fn an_excluded_devices_backlog_is_excluded_too() {
        let mut offline = device(1, 10, "Windows Server 2022");
        offline.offline = Some(true);
        let by_id = HashMap::from([(1, &offline)]);
        let maps = maps();
        let current = vec![
            patch(1, "MANUAL", "CRITICAL", Some(90)),
            patch(1, "APPROVED", "IMPORTANT", Some(90)),
        ];
        let counts = pending_counts(&refs(&current));
        let summaries = build_device_summaries(&[&offline], &counts, &maps);
        let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
        assert!(
            buckets.is_empty(),
            "a bucket with no devices in it must not be emitted at all, \
             let alone one reporting 100% beside a backlog: {buckets:?}"
        );
    }

    /// A patch whose device isn't in the scoped inventory has no device to be
    /// non-compliant, so it must not open a bucket of its own. It used to create an
    /// `(unknown)` organization with zero devices and — via `pct()`'s empty-bucket
    /// rule — 100% compliance.
    #[test]
    fn an_orphan_patch_does_not_invent_an_organization() {
        let d = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d)]);
        let maps = maps();
        let mut orphan = patch(1, "MANUAL", "CRITICAL", Some(90));
        orphan.device_id = Some(999); // not in the scoped inventory
        let current = vec![orphan];
        let counts = pending_counts(&refs(&current));
        let summaries = build_device_summaries(&[&d], &counts, &maps);
        let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
        assert_eq!(buckets.len(), 1, "only the real organization: {buckets:?}");
        assert_eq!(buckets[0].pending_critical, 0);
    }

    /// A current-feed record with no `status` is pending by construction — that feed
    /// is defined as the patches with no installation attempt — and `status` is not a
    /// required property. Dropping it understated the backlog and *raised* the
    /// compliance percentage.
    #[test]
    fn a_current_patch_with_no_status_still_counts_as_pending() {
        let d = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d)]);
        let maps = maps();
        let mut untyped = patch(1, "MANUAL", "CRITICAL", Some(90));
        untyped.status = None;
        let current = vec![untyped];
        let counts = pending_counts(&refs(&current));
        assert_eq!(counts.get(&1).copied(), Some(1));
        let summaries = build_device_summaries(&[&d], &counts, &maps);
        let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
        assert_eq!(buckets[0].devices_compliant, 0);
        assert_eq!(buckets[0].pending_critical, 1);
        // 90 days old → the "61-90 days" bucket.
        assert_eq!(
            build_age_buckets(&refs(&current), &by_id, Utc::now())[2].count,
            1
        );
    }

    /// Rounding must never manufacture a clean fleet.
    #[test]
    fn a_compliance_percentage_never_rounds_up_to_a_hundred() {
        assert_eq!(format_pct(100.0), "100%");
        assert_eq!(format_pct(99.9), "99%");
        assert_eq!(format_pct(99.5), "99%");
        assert_eq!(format_pct(94.6), "95%");
        assert_eq!(format_pct(0.4), "0%");
        // Same rule at the workbook's one decimal.
        let cell = |pct| match pct_cell(pct) {
            TableCell::Number(n) => n,
            _ => unreachable!("pct_cell always yields a number"),
        };
        assert!((cell(100.0) - 100.0).abs() < 1e-9);
        assert!((cell(99.99) - 99.9).abs() < 1e-9);
    }

    #[test]
    fn compliance_excludes_offline_devices_from_the_denominator() {
        let online = device(1, 10, "Windows Server 2022"); // online, has a pending patch
        let mut offline = device(2, 10, "Windows Server 2019");
        offline.offline = Some(true); // offline → unknown, must not count
        let by_id = HashMap::from([(1, &online), (2, &offline)]);
        let maps = maps();
        let current = vec![patch(1, "MANUAL", "CRITICAL", Some(1))];
        let counts = pending_counts(&refs(&current));
        let summaries = build_device_summaries(&[&online, &offline], &counts, &maps);
        let buckets = build_compliance(&summaries, &refs(&current), &by_id, &maps, 30, Utc::now());
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
    fn compliance_by_os_groups_devices_and_patches_by_os() {
        let d1 = device(1, 10, "Windows Server 2022"); // pending → not compliant
        let d2 = device(2, 10, "Windows 11 Pro"); // no pending → compliant
        let by_id = HashMap::from([(1, &d1), (2, &d2)]);
        let maps = maps();
        let current = vec![patch(1, "MANUAL", "CRITICAL", Some(45))]; // aged, on d1
        let counts = pending_counts(&refs(&current));
        let summaries = build_device_summaries(&[&d1, &d2], &counts, &maps);
        let buckets = build_compliance_by_os(&summaries, &refs(&current), &by_id, 30, Utc::now());
        assert_eq!(buckets.len(), 2, "one bucket per distinct OS");
        // Sorted by OS name (case-insensitive): "Windows 11 Pro" before "Windows Server 2022".
        let win11 = &buckets[0];
        assert_eq!(win11.os, "Windows 11 Pro");
        assert_eq!(win11.devices_total, 1);
        assert_eq!(win11.devices_compliant, 1);
        assert_eq!(win11.pending_critical, 0);
        assert!((win11.compliance_pct - 100.0).abs() < 1e-9);
        let server = &buckets[1];
        assert_eq!(server.os, "Windows Server 2022");
        assert_eq!(server.devices_total, 1);
        assert_eq!(
            server.devices_compliant, 0,
            "the device has a pending patch"
        );
        assert_eq!(server.pending_critical, 1);
        assert_eq!(
            server.aged_critical, 1,
            "released 45d ago, past the 30d SLA"
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default().prepare(),
        );
        let counts = pending_counts(&refs(&patches));
        let devices = build_device_summaries(&[&d], &counts, &maps);
        let compliance = build_compliance(&devices, &refs(&patches), &by_id, &maps, 30, Utc::now());
        let result = QueryResult {
            rows,
            devices,
            compliance,
            compliance_by_os: Vec::new(),
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 1,
            devices_offline: 0,
            patch_families: PatchFamilies {
                os: true,
                software: true,
            },
            scope: Default::default(),
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default().prepare(),
        );
        let counts = pending_counts(&refs(&patches));
        let devices = build_device_summaries(&[&d1, &d2], &counts, &maps);
        let compliance = build_compliance(&devices, &refs(&patches), &by_id, &maps, 30, Utc::now());
        let result = QueryResult {
            rows,
            devices,
            compliance,
            compliance_by_os: Vec::new(),
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 2,
            devices_offline: 0,
            patch_families: PatchFamilies {
                os: true,
                software: true,
            },
            scope: Default::default(),
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default().prepare(),
        );

        assert_eq!(rows.len(), 2);
        let mapped = rows.iter().find(|r| r.device_id == 1).unwrap();
        assert_eq!(
            &*mapped.organization, "(unknown)",
            "an org id absent from the lookup map renders as (unknown)"
        );
        assert_eq!(&*mapped.device_name, "srv1");
        let orphan = rows.iter().find(|r| r.device_id == 404).unwrap();
        assert_eq!(
            &*orphan.device_name, "(unknown)",
            "a patch for a device not in inventory has no resolvable name"
        );
        assert_eq!(&*orphan.organization, "(unknown)");
    }

    /// Rows that report the same value must *share* it, not each own a copy.
    ///
    /// This is the whole point of the interner, and it is invisible in every other
    /// assertion — a row set that duplicated every string would satisfy all of them
    /// while costing the cached result an allocation per field per row. On a fleet
    /// where one patch is missing from thousands of devices, and every device in an
    /// org repeats its org name, that difference is most of the result's memory.
    #[test]
    fn rows_share_one_allocation_per_distinct_string() {
        let maps = maps();
        let devices = [device(1, 10, "Windows Server 2022")];
        let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();
        // The same device and the same patch identity, twice.
        let patches = vec![
            patch(1, "MANUAL", "CRITICAL", Some(1)),
            patch(1, "MANUAL", "CRITICAL", Some(1)),
        ];
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default().prepare(),
        );
        assert_eq!(rows.len(), 2);
        let (a, b) = (&rows[0], &rows[1]);
        for (what, x, y) in [
            ("organization", &a.organization, &b.organization),
            ("device_name", &a.device_name, &b.device_name),
            ("name", &a.name, &b.name),
            ("status", &a.status, &b.status),
        ] {
            assert!(
                Arc::ptr_eq(x, y),
                "`{what}` must be one shared allocation across rows, not a copy each"
            );
        }
        assert!(Arc::ptr_eq(
            a.os_name.as_ref().unwrap(),
            b.os_name.as_ref().unwrap()
        ));
    }

    #[test]
    fn empty_inputs_yield_no_rows_or_compliance() {
        let maps = maps();
        let by_id: HashMap<i64, &Device> = HashMap::new();
        let rows = build_rows(&by_id, &maps, &[], &FilterParams::default().prepare());
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
                patches: &refs(&patches),
                type_label: "OS",
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default().prepare(),
        );
        assert_keys_present(
            &serde_json::to_value(&rows[0]).unwrap(),
            &[
                // The row's identity for action selection — the frontend keys its
                // checkboxes on this, so dropping it silently breaks selection.
                "deviceId",
                "deviceName",
                "organization",
                "location",
                "deviceRole",
                "osName",
                "offline",
                "patchType",
                "kb",
                "name",
                "severity",
                "status",
                "firstSeenDate",
                "installedDate",
            ],
            "PatchRow",
        );

        // The frontend mirror (`web-rs/src/types.rs`) declares these as `String` /
        // `Option<String>`, and the two crates share no code — nothing but this
        // checks that the wire still carries strings. The backend fields behind them
        // are now a mix of `String`, `Arc<str>` and `&'static str` so that a row set
        // shares its repeated values; all three must serialize identically, and a
        // future field that stops doing so has to fail here rather than at runtime in
        // a webview.
        let row_json = serde_json::to_value(&rows[0]).unwrap();
        for key in [
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
            "firstSeenDate",
        ] {
            let value = &row_json[key];
            assert!(
                value.is_string(),
                "PatchRow.{key} must reach the frontend as a JSON string, got {value}"
            );
        }
        assert!(
            row_json["deviceId"].is_i64() && row_json["offline"].is_boolean(),
            "the non-string row fields must keep their JSON types too"
        );

        let summaries = build_device_summaries(&[&d], &pending_counts(&refs(&patches)), &maps);
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

        let compliance =
            build_compliance(&summaries, &refs(&patches), &by_id, &maps, 30, Utc::now());
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

        let by_os = build_compliance_by_os(&summaries, &refs(&patches), &by_id, 30, Utc::now());
        assert_keys_present(
            &serde_json::to_value(&by_os[0]).unwrap(),
            &[
                "os",
                "devicesTotal",
                "devicesCompliant",
                "compliancePct",
                "pendingCritical",
                "agedCritical",
            ],
            "OsCompliance",
        );

        let result = QueryResult {
            rows,
            devices: summaries,
            compliance,
            compliance_by_os: Vec::new(),
            failures: Vec::new(),
            severity_by_org: Vec::new(),
            age_buckets: Vec::new(),
            devices_total: 1,
            devices_offline: 0,
            patch_families: PatchFamilies {
                os: true,
                software: true,
            },
            scope: Default::default(),
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
                "complianceByOs",
                "failures",
                "severityByOrg",
                "ageBuckets",
                "devicesTotal",
                "devicesOffline",
                "patchFamilies",
                "generatedAt",
                "dataFetchedAt",
            ],
            "QuerySummary",
        );
    }

    /// The sentence every compliance surface prints beside its percentages. It has to
    /// name both things a bare percentage hides: the excluded devices and the patch
    /// families actually counted.
    #[test]
    fn the_scope_note_states_the_population_and_the_families() {
        let both = PatchFamilies {
            os: true,
            software: true,
        };
        assert_eq!(
            compliance_scope_note(0, both),
            "Compliance covers online devices only."
        );
        assert_eq!(
            compliance_scope_note(1, both),
            "Compliance covers online devices only (1 offline device excluded)."
        );
        assert_eq!(
            compliance_scope_note(
                12,
                PatchFamilies {
                    os: true,
                    software: false
                }
            ),
            "Compliance covers online devices only (12 offline devices excluded), \
             and counts OS patches only."
        );
    }

    fn sortable_row(device: &str, sev_rank: u8, installed_ts: Option<i64>) -> PatchRow {
        PatchRow {
            severity_rank: sev_rank,
            ..failed_row(1, device, "KB1", installed_ts)
        }
    }

    /// A row on `device`, carrying `name`/`kb` so grouping can be exercised both ways.
    fn group_row(device_id: i64, device: &str, kb: Option<&str>, name: &str, rank: u8) -> PatchRow {
        PatchRow {
            device_id,
            device_name: device.into(),
            kb: kb.map(Into::into),
            name: name.into(),
            severity_rank: rank,
            patch_type: if kb.is_some() { "OS" } else { "SOFTWARE" },
            ..failed_row(device_id, device, "KB1", None)
        }
    }

    #[test]
    fn build_groups_by_device_rolls_up_rows_and_worst_severity() {
        let rows = vec![
            group_row(1, "web-01", Some("KB1"), "Cumulative Update", 3),
            group_row(1, "web-01", None, "Google Chrome 138", 7),
            group_row(2, "web-02", Some("KB1"), "Cumulative Update", 4),
        ];
        let groups = build_groups(&rows, GroupBy::Device);
        assert_eq!(groups.len(), 2, "one group per device");

        // Highest severity in the group wins, so a collapsed row still reads as
        // urgent as its worst member — and that ordering puts web-01 first.
        assert_eq!(&*groups[0].label, "web-01");
        assert_eq!(groups[0].severity_rank, 7);
        assert_eq!(groups[0].rows, 2);
        assert_eq!(groups[0].devices, 1, "a device group is exactly one device");
        assert_eq!(groups[0].device_id, Some(1));
        assert_eq!(&*groups[1].label, "web-02");
    }

    #[test]
    fn build_groups_by_patch_leads_with_blast_radius() {
        let rows = vec![
            // A critical patch on one device...
            group_row(1, "web-01", Some("KB9"), "Rare Critical", 7),
            // ...versus a less severe one missing on three.
            group_row(1, "web-01", None, "Google Chrome 138", 3),
            group_row(2, "web-02", None, "Google Chrome 138", 3),
            group_row(3, "web-03", None, "Google Chrome 138", 3),
        ];
        let groups = build_groups(&rows, GroupBy::Patch);
        assert_eq!(groups.len(), 2);
        // Blast radius leads: "missing on 3 machines" outranks "critical on 1".
        assert_eq!(&*groups[0].label, "Google Chrome 138");
        assert_eq!(groups[0].devices, 3);
        assert_eq!(groups[0].rows, 3);
        assert_eq!(
            groups[0].sublabel, None,
            "third-party patches carry no KB, so the sublabel stays empty"
        );
        assert_eq!(&*groups[1].label, "Rare Critical");
        assert_eq!(groups[1].sublabel.as_deref(), Some("KB9"));
        assert_eq!(groups[1].device_id, None, "a patch group spans devices");
    }

    /// The matcher reimplements `group_key`'s encoding, so the two must agree for
    /// every row and both groupings — otherwise expanding a group silently returns
    /// the wrong members (or none).
    #[test]
    fn group_key_and_matcher_agree() {
        let rows = vec![
            group_row(1, "web-01", Some("KB1"), "Cumulative Update", 5),
            group_row(2, "web-02", Some("KB1"), "Cumulative Update", 5),
            group_row(1, "web-01", None, "Google Chrome 138", 3),
            group_row(3, "db-01", Some("KB2"), "Security Update", 7),
        ];
        for group_by in [GroupBy::Device, GroupBy::Patch] {
            for row in &rows {
                let key = group_key(row, group_by);
                let matcher = GroupKeyMatcher::new(group_by, &key);
                for other in &rows {
                    assert_eq!(
                        matcher.matches(other),
                        group_key(other, group_by) == key,
                        "matcher disagreed with group_key for {:?} on {:?}",
                        other.name,
                        group_by
                    );
                }
            }
        }
    }

    /// A key the frontend echoed back from a previous result must match nothing
    /// rather than panicking or matching everything.
    #[test]
    fn an_unparseable_group_key_matches_nothing() {
        let rows = [
            group_row(1, "web-01", Some("KB1"), "Cumulative Update", 5),
            group_row(2, "web-02", None, "Google Chrome 138", 3),
        ];
        for (group_by, key) in [
            (GroupBy::Device, "not-a-number"),
            (GroupBy::Patch, "too\u{1f}few"),
            (GroupBy::Patch, "a\u{1f}b\u{1f}c\u{1f}d"),
        ] {
            let matcher = GroupKeyMatcher::new(group_by, key);
            assert!(
                !rows.iter().any(|r| matcher.matches(r)),
                "{key:?} must match nothing under {group_by:?}"
            );
        }
    }

    #[test]
    fn group_members_returns_only_that_groups_rows() {
        let rows = vec![
            group_row(1, "web-01", Some("KB1"), "Cumulative Update", 5),
            group_row(2, "web-02", Some("KB1"), "Cumulative Update", 5),
            group_row(1, "web-01", None, "Google Chrome 138", 3),
        ];
        // A patch group's members are the affected devices...
        let key = group_key(&rows[0], GroupBy::Patch);
        let members = group_member_page(&rows, GroupBy::Patch, &key, 0, 10);
        assert_eq!(members.len(), 2);
        assert!(members.iter().all(|r| &*r.name == "Cumulative Update"));

        // ...and a device group's members are that device's patches.
        let key = group_key(&rows[0], GroupBy::Device);
        let members = group_member_page(&rows, GroupBy::Device, &key, 0, 10);
        assert_eq!(members.len(), 2);
        assert!(members.iter().all(|r| r.device_id == 1));

        // Paging and a stale/unknown key both behave.
        assert_eq!(
            group_member_page(&rows, GroupBy::Device, &key, 1, 10).len(),
            1
        );
        assert!(group_member_page(&rows, GroupBy::Device, "nope", 0, 10).is_empty());
    }

    #[test]
    fn group_keys_cannot_collide_across_distinct_patches() {
        // The key joins patch_type/kb/name; a name containing the joiner would
        // otherwise be able to impersonate another group's key.
        let a = group_row(1, "web-01", Some("KB1"), "Update", 5);
        let b = group_row(1, "web-01", None, "KB1\u{1f}Update", 5);
        assert_ne!(group_key(&a, GroupBy::Patch), group_key(&b, GroupBy::Patch));
    }

    #[test]
    fn group_page_slices_and_reports_the_total() {
        let rows: Vec<PatchRow> = (1..=5)
            .map(|i| group_row(i, &format!("srv{i}"), Some("KB1"), "Cumulative Update", 5))
            .collect();
        let all = build_groups(&rows, GroupBy::Device);
        let page = slice_groups(&all, 2, 2);
        assert_eq!(page.total, 5, "total counts every group, not the page");
        assert_eq!(page.groups.len(), 2);
        assert!(
            slice_groups(&all, 99, 2).groups.is_empty(),
            "an offset past the end is an empty page, not a panic"
        );
        assert_eq!(
            slice_groups(&all, 99, 2).total,
            5,
            "and the total still describes the whole grouping"
        );
    }

    #[test]
    fn page_rows_without_sort_matches_cache_order() {
        let rows: Vec<PatchRow> = (0..5)
            .map(|i| failed_row(i, &format!("srv{i}"), "KB1", None))
            .collect();
        let page = sorted_page(&rows, 1, 2, None);
        assert_eq!(page.len(), 2);
        assert_eq!(&*page[0].device_name, "srv1");
        assert_eq!(&*page[1].device_name, "srv2");
        assert!(
            sorted_page(&rows, 10, 2, None).is_empty(),
            "offset past end"
        );
    }

    /// Every `RowSortKey` variant round-trips: each sorts ascending, and reverses
    /// under `desc`. Only 3 of the 12 were covered, so a key wired to the wrong
    /// field — or one added without a `compare_rows` arm — went unnoticed.
    #[test]
    fn every_sort_key_orders_and_reverses() {
        // `lo` and `hi` differ in exactly one field, and `lo` must come first when
        // ascending. `device_id` (1 = lo, 2 = hi) is the discriminator, so a key
        // wired to the wrong field is caught by position rather than by re-asking
        // the comparator under test.
        let base = |id: i64| PatchRow {
            device_id: id,
            ..failed_row(1, "dev", "KB1", None)
        };
        macro_rules! case {
            ($key:expr, $field:ident, $lo:expr, $hi:expr) => {{
                let mut l = base(1);
                l.$field = $lo;
                let mut h = base(2);
                h.$field = $hi;
                ($key, l, h)
            }};
        }

        let cases: Vec<(RowSortKey, PatchRow, PatchRow)> = vec![
            case!(
                RowSortKey::Organization,
                organization,
                "alpha".into(),
                "Beta".into()
            ),
            case!(
                RowSortKey::Location,
                location,
                Some("aisle".into()),
                Some("Bay".into())
            ),
            case!(
                RowSortKey::Role,
                device_role,
                Some("app".into()),
                Some("DB".into())
            ),
            case!(
                RowSortKey::Device,
                device_name,
                "alpha".into(),
                "Beta".into()
            ),
            case!(
                RowSortKey::Os,
                os_name,
                Some("alpine".into()),
                Some("Windows".into())
            ),
            case!(RowSortKey::PatchType, patch_type, "OS", "SOFTWARE"),
            case!(RowSortKey::Kb, kb, Some("KB1".into()), Some("KB2".into())),
            case!(RowSortKey::Name, name, "aardvark".into(), "Zebra".into()),
            // Ascending severity is most-urgent-first, so the HIGHER rank is `lo`.
            case!(RowSortKey::Severity, severity_rank, 7, 2),
            case!(
                RowSortKey::Status,
                status,
                "Approved".into(),
                "Failed".into()
            ),
            case!(
                RowSortKey::FirstSeenDate,
                first_seen_ts,
                Some(100),
                Some(200)
            ),
            case!(
                RowSortKey::InstalledDate,
                installed_ts,
                Some(100),
                Some(200)
            ),
        ];

        assert_eq!(
            cases.len(),
            12,
            "every RowSortKey variant needs a case here"
        );

        for (key, lo, hi) in cases {
            // Fed in reverse so an unsorted passthrough fails.
            let rows = vec![hi, lo];
            let ids = |desc: bool| -> Vec<i64> {
                sorted_page(&rows, 0, 10, Some(RowSort { key, desc }))
                    .iter()
                    .map(|r| r.device_id)
                    .collect()
            };
            assert_eq!(ids(false), vec![1, 2], "{key:?} did not order ascending");
            assert_eq!(ids(true), vec![2, 1], "{key:?} did not reverse under desc");
        }
    }

    /// `PatchType` and `Status` compare case-sensitively (byte order), unlike the
    /// name-ish keys. Both are backend-normalised values, so this pins the current
    /// behavior rather than leaving it accidental.
    #[test]
    fn patch_type_and_status_sort_by_byte_order() {
        let lower = PatchRow {
            patch_type: "os",
            ..failed_row(1, "a", "KB1", None)
        };
        let upper = PatchRow {
            patch_type: "OS",
            ..failed_row(2, "b", "KB1", None)
        };
        let sort = RowSort {
            key: RowSortKey::PatchType,
            desc: false,
        };
        assert_eq!(
            compare_rows(&upper, &lower, sort),
            Ordering::Less,
            "uppercase sorts before lowercase — byte order, not case-insensitive"
        );
    }

    #[test]
    fn page_rows_sorts_case_insensitively_then_slices() {
        let rows = vec![
            sortable_row("bravo", 5, None),
            sortable_row("Alpha", 5, None),
            sortable_row("charlie", 5, None),
        ];
        let sort = Some(RowSort {
            key: RowSortKey::Device,
            desc: false,
        });
        let names: Vec<_> = sorted_page(&rows, 0, 10, sort)
            .into_iter()
            .map(|r| r.device_name.to_string())
            .collect();
        assert_eq!(names, ["Alpha", "bravo", "charlie"]);
        // The offset/limit slice applies after the sort.
        assert_eq!(&*sorted_page(&rows, 1, 1, sort)[0].device_name, "bravo");
    }

    #[test]
    fn page_rows_desc_reverses_but_missing_values_stay_last() {
        let rows = vec![
            sortable_row("a", 5, Some(100)),
            sortable_row("b", 5, None),
            sortable_row("c", 5, Some(200)),
        ];
        let key = RowSortKey::InstalledDate;
        let names = |desc: bool| -> Vec<String> {
            sorted_page(&rows, 0, 10, Some(RowSort { key, desc }))
                .into_iter()
                .map(|r| r.device_name.to_string())
                .collect()
        };
        assert_eq!(names(false), ["a", "c", "b"]);
        assert_eq!(
            names(true),
            ["c", "a", "b"],
            "None still sorts last on desc"
        );
    }

    #[test]
    fn page_rows_severity_ascending_is_most_urgent_first() {
        let rows = vec![
            sortable_row("low", 2, None),
            sortable_row("crit", 5, None),
            sortable_row("mod", 3, None),
        ];
        let names: Vec<_> = sorted_page(
            &rows,
            0,
            10,
            Some(RowSort {
                key: RowSortKey::Severity,
                desc: false,
            }),
        )
        .into_iter()
        .map(|r| r.device_name.to_string())
        .collect();
        assert_eq!(names, ["crit", "mod", "low"]);
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
            offline: false,
            patch_type: "OS",
            kb: Some(kb.into()),
            name: "Cumulative Update".into(),
            severity: "Critical",
            severity_rank: 5,
            status: "FAILED".into(),
            first_seen_date: None,
            installed_date: installed_ts.map(|_| "2026-01-01 00:00 UTC".into()),
            first_seen_ts: None,
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

    fn scope_filter() -> FilterParams {
        FilterParams {
            organization_ids: Vec::new(),
            location_ids: Vec::new(),
            role_ids: Vec::new(),
            node_classes: Vec::new(),
            os_name_contains: None,
            search: None,
            severities: Vec::new(),
            detected_within_days: None,
            detected_after: None,
            detected_before: None,
        }
    }

    const BOTH_FAMILIES: PatchFamilies = PatchFamilies {
        os: true,
        software: true,
    };

    fn facet<'a>(scope: &'a QueryScope, label: &str) -> Option<&'a str> {
        scope
            .facets
            .iter()
            .find(|(l, _)| *l == label)
            .map(|(_, v)| v.as_str())
    }

    /// An unfiltered export must *say* it is unfiltered. On a printed artifact the
    /// absence of narrowing lines is indistinguishable from a renderer that dropped
    /// them, and the two readings differ by the whole fleet.
    #[test]
    fn an_unnarrowed_query_states_that_it_covers_the_whole_fleet() {
        let scope = build_query_scope(
            &scope_filter(),
            &maps(),
            BOTH_FAMILIES,
            &[PatchStatus::Pending],
            None,
        );
        assert_eq!(
            facet(&scope, "Scope"),
            Some("Whole fleet \u{2014} no device or patch filters applied")
        );
        // The two facets that always apply are still stated.
        assert_eq!(
            facet(&scope, "Patch type"),
            Some("OS and third-party patches")
        );
        assert_eq!(facet(&scope, "Status"), Some("Pending"));
        assert_eq!(
            facet(&scope, "Install history since"),
            None,
            "a Pending-only query never reached the history endpoints"
        );
    }

    /// Ids resolve to the names the operator picked them by, and an id the lookups
    /// can't resolve prints as `id N` — two unresolved ids rendering as
    /// "(unknown), (unknown)" would say neither how many were selected nor which.
    #[test]
    fn the_scope_block_names_every_active_facet() {
        let mut filter = scope_filter();
        filter.organization_ids = vec![10, 77];
        filter.location_ids = vec![100];
        filter.role_ids = vec![2];
        filter.node_classes = vec!["WINDOWS_SERVER".into()];
        filter.os_name_contains = Some("Server 2019".into());
        filter.severities = vec!["CRITICAL".into(), "IMPORTANT".into()];
        filter.search = Some("KB5040434".into());
        filter.detected_within_days = Some(30);
        filter.detected_after = Some(1_777_000_000);
        filter.detected_before = Some(1_779_000_000);

        let scope = build_query_scope(
            &filter,
            &maps(),
            PatchFamilies {
                os: true,
                software: false,
            },
            &[PatchStatus::Pending, PatchStatus::Failed],
            Some(1_776_000_000),
        );

        assert_eq!(facet(&scope, "Scope"), None, "something was narrowed");
        assert_eq!(facet(&scope, "Organizations"), Some("Contoso, id 77"));
        assert_eq!(facet(&scope, "Locations"), Some("HQ"));
        assert_eq!(facet(&scope, "Device roles"), Some("Domain Controller"));
        assert_eq!(facet(&scope, "OS type"), Some("WINDOWS_SERVER"));
        assert_eq!(facet(&scope, "OS name contains"), Some("Server 2019"));
        assert_eq!(facet(&scope, "Patch type"), Some("OS patches only"));
        assert_eq!(facet(&scope, "Status"), Some("Pending, Failed"));
        assert_eq!(facet(&scope, "Severity"), Some("CRITICAL, IMPORTANT"));
        assert_eq!(facet(&scope, "Search"), Some("KB5040434"));
        // Absolute, so it still means the same thing when the report is read months
        // later; the relative window the operator picked rides along in parentheses.
        assert_eq!(
            facet(&scope, "First seen after"),
            Some("2026-04-24 03:06 UTC (last 30 days)")
        );
        assert_eq!(
            facet(&scope, "First seen before"),
            Some("2026-05-17 06:40 UTC")
        );
        assert_eq!(
            facet(&scope, "Install history since"),
            Some("2026-04-12 13:20 UTC")
        );
    }

    /// A window entered as an absolute date carries no "(last N days)" tail — that
    /// parenthetical describes the control the operator used, not the bound.
    #[test]
    fn an_absolute_first_seen_bound_is_not_labelled_as_a_relative_window() {
        let mut filter = scope_filter();
        filter.detected_after = Some(1_777_000_000);
        let scope = build_query_scope(
            &filter,
            &maps(),
            BOTH_FAMILIES,
            &[PatchStatus::Pending],
            None,
        );
        assert_eq!(
            facet(&scope, "First seen after"),
            Some("2026-04-24 03:06 UTC")
        );
    }

    /// Every fleet-health rollup describes one population. The HTML report prints
    /// the compliance sections and these two charts in sequence under a header
    /// stating "Compliance covers online devices only (N offline devices excluded)",
    /// so a rollup that counts a wider set makes that sentence false for part of its
    /// own document — and the gap is exactly the excluded backlog, which is the one
    /// thing a reader cannot recover from the numbers on the page.
    #[test]
    fn severity_and_age_rollups_cover_the_same_devices_compliance_does() {
        let online = device(1, 10, "Windows Server 2022");
        let mut offline = device(2, 10, "Windows Server 2022");
        offline.offline = Some(true);
        let devices = [online, offline];
        let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();
        let maps = maps();

        // One pending Critical on the online device; three on the offline one, plus
        // an orphan with no device in inventory at all.
        let mut orphan = patch(1, "MANUAL", "CRITICAL", Some(5));
        orphan.device_id = Some(999);
        let current = vec![
            patch(1, "MANUAL", "CRITICAL", Some(5)),
            patch(2, "MANUAL", "CRITICAL", Some(5)),
            patch(2, "MANUAL", "CRITICAL", Some(5)),
            patch(2, "MANUAL", "CRITICAL", Some(5)),
            orphan,
        ];
        let current = refs(&current);

        let counts = pending_counts(&current);
        let summaries = build_device_summaries(&devices.iter().collect::<Vec<_>>(), &counts, &maps);
        let compliance = build_compliance(&summaries, &current, &by_id, &maps, 30, Utc::now());
        let severity = build_severity_by_org(&current, &by_id, &maps);
        let age = build_age_buckets(&current, &by_id, Utc::now());

        assert_eq!(
            compliance.len(),
            1,
            "one organization, not an (unknown) too"
        );
        assert_eq!(
            compliance[0].devices_total, 1,
            "the offline device is excluded"
        );
        assert_eq!(compliance[0].pending_critical, 1, "and so is its backlog");

        let severity_total: usize = severity.iter().map(|o| o.counts.total()).sum();
        assert_eq!(
            severity_total, compliance[0].pending_critical,
            "the severity breakdown counted the devices compliance excluded"
        );
        assert_eq!(
            severity.len(),
            1,
            "an orphan patch must not open its own (unknown) organization here \
             when compliance drops it"
        );
        let age_total: usize = age.iter().map(|b| b.count).sum();
        assert_eq!(
            age_total, compliance[0].pending_critical,
            "the age histogram counted them too"
        );
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
        let sev = build_severity_by_org(&refs(&current), &by_id, &maps);
        assert_eq!(sev.len(), 1);
        assert_eq!(&*sev[0].organization, "Contoso");
        assert_eq!(sev[0].counts.critical, 1);
        assert_eq!(sev[0].counts.important, 1);
        assert_eq!(sev[0].counts.moderate, 0);
    }

    #[test]
    fn build_age_buckets_separate_undated_patches_from_genuinely_old_ones() {
        let d = device(1, 10, "Windows Server 2022");
        let by_id = HashMap::from([(1, &d)]);
        let mut undated = patch(1, "MANUAL", "CRITICAL", Some(5));
        undated.collected_timestamp = None;
        let current = vec![
            patch(1, "MANUAL", "CRITICAL", Some(5)),   // 0-30
            patch(1, "MANUAL", "CRITICAL", Some(200)), // 180+
            undated,
            patch(1, "INSTALLED", "CRITICAL", Some(5)), // not pending → ignored
        ];
        let buckets = build_age_buckets(&refs(&current), &by_id, Utc::now());
        assert_eq!(buckets.len(), 6, "five age bands plus the undated bucket");
        assert_eq!(buckets[0].count, 1, "0-30 bucket");
        // The undated patch must NOT inflate 180+: folding it in made the tallest,
        // most alarming bar mean "we have no timestamp" rather than "this is old".
        assert_eq!(
            buckets[4].count, 1,
            "180+ holds only the genuinely aged one"
        );
        assert_eq!(&*buckets[5].label, "Unknown");
        assert_eq!(buckets[5].count, 1, "the undated patch lands in Unknown");
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
            build_severity_by_org(&[&patch(1, "MANUAL", "CRITICAL", Some(1))], &by_id, &maps());
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

        let buckets = build_age_buckets(
            &[&patch(1, "MANUAL", "CRITICAL", Some(1))],
            &by_id,
            Utc::now(),
        );
        assert_keys_present(
            &serde_json::to_value(&buckets[0]).unwrap(),
            &["label", "count"],
            "AgeBucket",
        );
    }
}
