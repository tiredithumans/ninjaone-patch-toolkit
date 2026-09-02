//! The device↔patch join: id→name lookups, the per-join string interner, the
//! per-device label bundle, and `build_rows`, which produces the flat
//! `PatchRow`s every table, export and rollup reads.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::filter::PreparedFilter;
use crate::model::{Device, Location, Organization, Patch, PatchRow, Role};

/// Placeholder for a name the join could not resolve — an orphan device, a device
/// reporting no OS, or a patch whose organization is not in the lookups.
pub(super) const UNKNOWN_LABEL: &str = "(unknown)";

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

    pub(super) fn org_name(&self, id: Option<i64>) -> String {
        self.org_name_str(id).to_string()
    }

    /// Borrowing form of [`org_name`](Self::org_name), for per-patch loops.
    pub(super) fn org_name_str(&self, id: Option<i64>) -> &str {
        id.and_then(|i| self.orgs.get(&i))
            .map(String::as_str)
            .unwrap_or(UNKNOWN_LABEL)
    }

    pub(super) fn location_name(&self, id: Option<i64>) -> Option<String> {
        id.and_then(|i| self.locations.get(&i)).cloned()
    }

    pub(super) fn role_name(&self, id: Option<i64>) -> Option<String> {
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

pub(super) fn fmt_dt(ts: Option<DateTime<Utc>>) -> Option<String> {
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
