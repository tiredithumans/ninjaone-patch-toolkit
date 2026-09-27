//! Per-device backlog lists: the worst online devices (the `rollup_device`
//! population, ranked) and the offline devices NinjaOne still holds pending
//! records for — the population every other rollup excludes.

use std::cmp::Ordering;
use std::collections::HashMap;

use chrono::DateTime;
use serde::Serialize;

use crate::model::{Device, Patch};

use super::compliance::{SlaCutoffs, rollup_device};
use super::join::fmt_dt;
use super::rollups::is_pending;
use super::*;

/// How many devices each list carries. Both ride on the summary whole, so they are
/// capped; the list's `devices_total` says how many qualified.
pub const DEVICE_BACKLOG_LIMIT: usize = 25;

/// One device's pending backlog.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBacklog {
    pub device_id: i64,
    pub device_name: String,
    pub organization: String,
    pub os_name: Option<String>,
    /// Pending records per band.
    pub pending: SeverityCounts,
    pub pending_total: usize,
    /// Pending records of **any** severity first seen before their own band's SLA
    /// cutoff (undated counts, as in the compliance rollups). Wider than the
    /// compliance tables' "Aged (past SLA)", which counts Critical/Important only.
    pub past_sla: usize,
    /// The earliest `timestamp` among the pending records — how long the oldest
    /// one has been known, not when it was released.
    pub oldest_first_seen: Option<String>,
    pub oldest_first_seen_ts: Option<i64>,
    /// The newest `timestamp` among the pending records: NinjaOne's "collected /
    /// updated" time, so the latest point this device's patch data is known to
    /// reflect. The device inventory this app reads carries no last-contact field,
    /// so this is the closest honest stand-in for an offline device.
    pub latest_collected: Option<String>,
    pub latest_collected_ts: Option<i64>,
}

/// A capped, ranked device list plus how many devices qualified in all.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBacklogList {
    pub devices: Vec<DeviceBacklog>,
    pub devices_total: usize,
}

#[derive(Default)]
struct Acc {
    pending: SeverityCounts,
    past_sla: usize,
    oldest: Option<i64>,
    latest: Option<i64>,
}

/// The worst online devices and the offline backlog, from one pass over the
/// current feed.
///
/// `worst` is the [`rollup_device`] population — the same devices the compliance
/// table counts — so its entries reconcile with the Compliance tab.
/// `offline` is its deliberate complement: scoped, patchable devices that are
/// offline yet still have pending records in NinjaOne's current feed. Those records
/// were collected before the device dropped off and may be stale, which is why the
/// fleet-health rollups exclude the device; listing them separately is what keeps
/// that backlog from vanishing altogether.
///
/// Both lists rank the same way — see [`rank`].
pub fn build_device_backlogs(
    current_patches: &[&Patch],
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
    sla: &SlaCutoffs,
) -> (DeviceBacklogList, DeviceBacklogList) {
    let mut online: HashMap<i64, Acc> = HashMap::new();
    let mut offline: HashMap<i64, Acc> = HashMap::new();
    for p in current_patches {
        let Some(id) = p.device_id.filter(|_| is_pending(p.status.as_deref())) else {
            continue;
        };
        let list = if rollup_device(devices_by_id, Some(id)).is_some() {
            &mut online
        } else if devices_by_id
            .get(&id)
            .is_some_and(|d| d.is_offline() && d.is_patchable())
        {
            &mut offline
        } else {
            continue;
        };
        let acc = list.entry(id).or_default();
        acc.pending.bump(p.severity_enum());
        if sla.is_aged(p) {
            acc.past_sla += 1;
        }
        if let Some(seen) = p.first_seen_at().map(|t| t.timestamp()) {
            acc.oldest = Some(acc.oldest.map_or(seen, |o| o.min(seen)));
            acc.latest = Some(acc.latest.map_or(seen, |l| l.max(seen)));
        }
    }
    (
        finish(online, devices_by_id, maps),
        finish(offline, devices_by_id, maps),
    )
}

fn finish(
    accs: HashMap<i64, Acc>,
    devices_by_id: &HashMap<i64, &Device>,
    maps: &LookupMaps,
) -> DeviceBacklogList {
    let devices_total = accs.len();
    let mut ranked: Vec<(i64, Acc)> = accs.into_iter().collect();
    ranked.sort_by(|(a_id, a), (b_id, b)| rank(a, *a_id, b, *b_id));
    ranked.truncate(DEVICE_BACKLOG_LIMIT);
    let when = |ts: Option<i64>| fmt_dt(ts.and_then(|t| DateTime::from_timestamp(t, 0)));
    let devices = ranked
        .into_iter()
        .filter_map(|(id, acc)| {
            let d = devices_by_id.get(&id)?;
            Some(DeviceBacklog {
                device_id: id,
                device_name: d.label().to_string(),
                organization: maps.org_name(d.organization_id),
                os_name: d.os_name(),
                pending_total: acc.pending.total(),
                pending: acc.pending,
                past_sla: acc.past_sla,
                oldest_first_seen: when(acc.oldest),
                oldest_first_seen_ts: acc.oldest,
                latest_collected: when(acc.latest),
                latest_collected_ts: acc.latest,
            })
        })
        .collect();
    DeviceBacklogList {
        devices,
        devices_total,
    }
}

/// Worst first: most records past SLA, then the most urgent breakdown
/// ([`SeverityCounts::cmp_urgency`] — more Criticals wins, then Important, …),
/// then lowest device id. That breakdown order already implies the larger total
/// wherever the bands differ, so total pending needs no step of its own; the id
/// makes the order total, because the accumulator is a `HashMap` and two identical
/// runs must list tied devices identically.
fn rank(a: &Acc, a_id: i64, b: &Acc, b_id: i64) -> Ordering {
    b.past_sla
        .cmp(&a.past_sla)
        .then_with(|| b.pending.cmp_urgency(&a.pending))
        .then_with(|| a_id.cmp(&b_id))
}

impl DeviceBacklog {
    /// The worst-devices columns, shared by the workbook and the report.
    pub const WORST_COLUMNS: [TableColumn<DeviceBacklog>; 7] = [
        ("Organization", |d| TableCell::text(&d.organization)),
        ("Device", |d| TableCell::text(&d.device_name)),
        ("OS", |d| TableCell::opt_text(d.os_name.as_deref())),
        ("Past SLA", |d| TableCell::Count(d.past_sla)),
        ("Pending Patches", |d| TableCell::Count(d.pending_total)),
        ("Pending by Severity", |d| {
            TableCell::Text(d.pending.breakdown())
        }),
        ("Oldest First Seen", |d| {
            TableCell::opt_text(d.oldest_first_seen.as_deref())
        }),
    ];

    /// The offline-backlog columns. Leads with when the data was collected, because
    /// for an offline device that is what decides how far to trust the rest.
    pub const OFFLINE_COLUMNS: [TableColumn<DeviceBacklog>; 7] = [
        ("Organization", |d| TableCell::text(&d.organization)),
        ("Device", |d| TableCell::text(&d.device_name)),
        ("OS", |d| TableCell::opt_text(d.os_name.as_deref())),
        ("Latest Patch Data Collected", |d| {
            TableCell::opt_text(d.latest_collected.as_deref())
        }),
        ("Pending Patches", |d| TableCell::Count(d.pending_total)),
        ("Past SLA", |d| TableCell::Count(d.past_sla)),
        ("Pending by Severity", |d| {
            TableCell::Text(d.pending.breakdown())
        }),
    ];
}

/// Printed under both device lists wherever they are rendered.
pub const WORST_DEVICES_NOTE: &str = "Online Windows, macOS and Linux devices in scope, \
     ranked by pending patches past their severity's SLA (any severity), then by the most \
     urgent backlog.";

/// Printed under the offline list: what the data can and cannot say about it.
pub const OFFLINE_BACKLOG_NOTE: &str = "Offline devices still listed with pending patches in \
     NinjaOne's current feed. These records were collected before the device went offline \
     and may be stale; the compliance figures exclude these devices. \"Latest patch data \
     collected\" is the newest collection time on the device's records, not a last-contact \
     time.";
