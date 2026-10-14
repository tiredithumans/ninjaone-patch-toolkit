//! The per-record view every rollup over the current feed reads.

use chrono::{DateTime, Utc};

use crate::model::{Patch, Severity};

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
}

impl<'a> PatchRecord<'a> {
    /// A record dated by NinjaOne's own time alone.
    pub fn vendor(patch: &'a Patch) -> Self {
        Self {
            patch,
            severity: patch.severity_enum(),
            pending: is_pending(patch.status.as_deref()),
            first_seen: patch.collected_at(),
        }
    }

    /// One view per record, in order.
    pub fn vendor_all(patches: &[&'a Patch]) -> Vec<Self> {
        patches.iter().map(|p| Self::vendor(p)).collect()
    }
}
