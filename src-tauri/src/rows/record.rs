//! The per-record view every rollup over the current feed reads.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;

use crate::model::{Device, Patch, Severity};

use super::compliance::rollup_device;
use super::join::fmt_dt;
use super::rollups::is_pending;

/// One patch record as the rollups see it: the vendor record plus what this query
/// decided about it, worked out once per record rather than once per rollup.
///
/// The point is `first_seen`. NinjaOne's own time on a record
/// ([`Patch::collected_at`]) is "when data was collected/updated", which may move on
/// every scan; every age a rollup reports — past SLA, the age histogram, stuck
/// approvals, a row's First Seen and the first-seen filter — reads this field
/// instead, so the one decision about *which* time a record is aged from is made
/// here and nowhere else. Severity and the pending verdict ride along because each
/// rollup re-derived them per record before.
#[derive(Debug, Clone, Copy)]
pub struct PatchRecord<'a> {
    pub patch: &'a Patch,
    pub severity: Severity,
    /// [`is_pending`] on the vendor status.
    pub pending: bool,
    /// When this record was first seen, from whichever source says so earliest.
    /// `None` when nothing dates it — which every rollup treats as "cannot be shown
    /// to be recent".
    pub first_seen: Option<DateTime<Utc>>,
    /// Whether `first_seen` is this app's own observation, strictly earlier than
    /// NinjaOne's time — the count of these is how an operator learns whether
    /// NinjaOne's times reset on each scan.
    pub first_seen_from_app: bool,
}

impl<'a> PatchRecord<'a> {
    /// A record dated by NinjaOne's own time alone.
    pub fn vendor(patch: &'a Patch) -> Self {
        Self {
            patch,
            severity: patch.severity_enum(),
            pending: is_pending(patch.status.as_deref()),
            first_seen: patch.collected_at(),
            first_seen_from_app: false,
        }
    }

    /// This record dated by the earlier of its current time and when this app first
    /// saw it pending (`app`, from `crate::first_seen`).
    ///
    /// A record NinjaOne never dated stays undated even if this app has seen it: an
    /// undated record already counts as past SLA, and dating it would only make it
    /// *younger*. So the app's record can make a record older, never newer, and can
    /// never lower a past-SLA count.
    pub fn observed(self, app: Option<DateTime<Utc>>) -> Self {
        match (self.first_seen, app) {
            (Some(theirs), Some(ours)) if ours < theirs => Self {
                first_seen: Some(ours),
                first_seen_from_app: true,
                ..self
            },
            _ => self,
        }
    }

    /// One view per record, in order.
    pub fn vendor_all(patches: &[&'a Patch]) -> Vec<Self> {
        patches.iter().map(|p| Self::vendor(p)).collect()
    }
}

/// How a result's first-seen times were arrived at, for every surface that defines
/// "first seen": since when this app has kept its own record, and how many of the
/// pending records the rollups count are dated by that record rather than by
/// NinjaOne's time. A count that climbs from run to run is NinjaOne's times
/// resetting — the evidence for which kind of time the vendor sends.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstSeenTracking {
    /// When this app began recording first-seen times for this tenant and these
    /// patch families, formatted like the row dates; `None` when no family is in
    /// the result.
    pub since: Option<String>,
    pub since_ts: Option<i64>,
    /// Pending records on the counted devices (`rollup_device`).
    pub pending_records: usize,
    /// Of those, the ones dated by this app's record — earlier than NinjaOne's time.
    pub from_app_record: usize,
}

impl FirstSeenTracking {
    /// Counts over the rollup population, so the figure describes the records the
    /// compliance numbers are built from.
    pub fn build(
        current: &[PatchRecord<'_>],
        devices_by_id: &HashMap<i64, &Device>,
        since: Option<DateTime<Utc>>,
    ) -> Self {
        let counted = current
            .iter()
            .filter(|r| r.pending && rollup_device(devices_by_id, r.patch.device_id).is_some());
        let (mut pending_records, mut from_app_record) = (0, 0);
        for r in counted {
            pending_records += 1;
            from_app_record += usize::from(r.first_seen_from_app);
        }
        Self {
            since: fmt_dt(since),
            since_ts: since.map(|t| t.timestamp()),
            pending_records,
            from_app_record,
        }
    }
}
