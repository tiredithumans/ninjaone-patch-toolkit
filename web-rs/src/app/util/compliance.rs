//! The Compliance tab's own sentences: what its figures mean and what they leave
//! out. Each mirrors a backend `rows::` function, so the in-app wording and the
//! exports' read the same.

use crate::types::{ComplianceBucket, FirstSeenTracking, PatchFamilies, SlaExempt, SlaPolicy};

use super::{format_pct, group_thousands, sla_policy_summary};

/// How many organizations [`uncounted_orgs_note`] names before "and N more".
pub(crate) const UNCOUNTED_ORGS_LIMIT: usize = 10;

/// The line under the organization table naming the organizations that have no row
/// in it — every device offline or not patchable — or `None` when there are none.
/// Without it such an organization silently vanishes from the table.
pub(crate) fn uncounted_orgs_note(orgs: &[String]) -> Option<String> {
    match orgs {
        [] => None,
        [one] => Some(format!(
            "{one} has no online, patchable device in scope, so it is not in the table."
        )),
        _ => {
            let shown = orgs[..orgs.len().min(UNCOUNTED_ORGS_LIMIT)].join(", ");
            let more = orgs.len().saturating_sub(UNCOUNTED_ORGS_LIMIT);
            let tail = if more > 0 {
                format!(", and {more} more")
            } else {
                String::new()
            };
            Some(format!(
                "{} organizations have no online, patchable device in scope, so they are not \
                 in the table: {shown}{tail}.",
                orgs.len()
            ))
        }
    }
}

/// What "first seen" means in this result: the earlier of NinjaOne's time and this
/// app's own record, since when that record has been kept, and how many pending
/// records it dated. `None` when no patch family is in the result. Mirrors
/// `rows::FirstSeenTracking::sentence`.
pub(crate) fn first_seen_note(t: &FirstSeenTracking) -> Option<String> {
    let since = t.since.as_deref()?;
    let dated = match t.from_app_record {
        0 => "every pending record here is dated by NinjaOne's time".to_string(),
        n => format!(
            "{} of {} pending records are dated by this app's record, because NinjaOne's \
             time on them is later",
            group_thousands(n),
            group_thousands(t.pending_records)
        ),
    };
    Some(format!(
        "The earlier of NinjaOne's record time and when this app first saw the patch pending \
         (this app's record began {since}); {dated}."
    ))
}

/// Printed under the worst-devices list. Mirrors `rows::WORST_DEVICES_NOTE`.
pub(crate) const WORST_DEVICES_NOTE: &str = "Online Windows, macOS and Linux devices in scope, \
     ranked by pending patches past their severity's SLA (every severity that has one), then \
     by the most urgent backlog.";

/// One headline figure. Mirrors `rows::HeadlineFigure`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Kpi {
    pub label: &'static str,
    pub value: String,
    pub detail: String,
    pub definition: &'static str,
}

/// The Compliance tab's four headline figures for a result's fleet total. Mirrors
/// `rows::headline_figures`: a percentage over no counted device is "—", never the
/// 100% an empty bucket computes.
pub(crate) fn compliance_kpis(
    total: &ComplianceBucket,
    devices_offline: usize,
    devices_unpatchable: usize,
) -> [Kpi; 4] {
    let n = total.devices_total;
    let pct = |p: f64| {
        if n == 0 {
            "\u{2014}".to_string()
        } else {
            format_pct(p)
        }
    };
    let of = |k: usize| {
        if n == 0 {
            "no counted devices".to_string()
        } else {
            let devices = if n == 1 { "device" } else { "devices" };
            format!("{} of {} {devices}", group_thousands(k), group_thousands(n))
        }
    };
    let past = match total.urgent_past_sla {
        0 => "none past SLA".to_string(),
        k => format!("{} past SLA", group_thousands(k)),
    };
    let excluded = match (devices_offline, devices_unpatchable) {
        (0, 0) => "none".to_string(),
        (o, 0) => format!("{} offline", group_thousands(o)),
        (0, u) => format!("{} non-patchable", group_thousands(u)),
        (o, u) => format!(
            "{} offline, {} non-patchable",
            group_thousands(o),
            group_thousands(u)
        ),
    };
    [
        Kpi {
            label: "Within SLA",
            value: pct(total.within_sla_pct),
            detail: of(total.devices_within_sla),
            definition: "Devices with no pending patch past its severity's SLA window.",
        },
        Kpi {
            label: "Fully patched",
            value: pct(total.fully_patched_pct),
            detail: of(total.devices_fully_patched),
            definition: "Devices with no pending patch of any severity or age.",
        },
        Kpi {
            label: "Urgent pending",
            value: group_thousands(total.urgent_pending),
            detail: past,
            definition: "Critical, Important and Security patches not yet installed.",
        },
        Kpi {
            label: "Excluded devices",
            value: group_thousands(devices_offline + devices_unpatchable),
            detail: excluded,
            definition: "Offline, or not patchable by NinjaOne, so in no figure here.",
        },
    ]
}

/// The "How these numbers are calculated" definitions, as (term, meaning), from the
/// result's own policy, patch families and first-seen record. Mirrors
/// `rows::calculation_notes`.
pub(crate) fn calculation_notes(
    policy: &SlaPolicy,
    families: PatchFamilies,
    first_seen: &FirstSeenTracking,
) -> Vec<(&'static str, String)> {
    let any_exempt = SlaExempt::BANDS
        .iter()
        .any(|(_, get, _)| get(&policy.exempt));
    let no_sla = if any_exempt {
        " A severity with No SLA is never past SLA, but its patches still count as pending."
    } else {
        ""
    };
    let first_seen = first_seen_note(first_seen).unwrap_or_else(|| {
        "NinjaOne's record time (when it last collected the record).".to_string()
    });
    vec![
        (
            "Counted devices",
            "Online Windows, macOS and Linux devices in the selected scope. Offline devices are \
             left out because their patch records may be out of date, and devices NinjaOne \
             cannot patch (switches, printers, hypervisors, cloud monitors) have none, so \
             neither is counted as clean."
                .to_string(),
        ),
        (
            "Pending",
            format!(
                "A current patch record NinjaOne has not marked Installed or Rejected: awaiting \
                 approval, approved, failed, or with no status. Patch families counted: {}.",
                families.label()
            ),
        ),
        (
            "Within SLA",
            "A counted device with no pending patch past its severity's SLA window.".to_string(),
        ),
        (
            "Fully patched",
            "A counted device with no pending patch at all, of any severity or age.".to_string(),
        ),
        (
            "Urgent",
            "Critical, Important and Security patches. NinjaOne grades most monthly security \
             updates Security rather than Critical or Important."
                .to_string(),
        ),
        (
            "Past SLA",
            format!(
                "Pending for longer than its severity's SLA window since first seen; a patch with \
                 no first-seen time counts as past SLA. SLA policy: {}.{no_sla}",
                sla_policy_summary(policy)
            ),
        ),
        ("First seen", first_seen),
        (
            "Approval columns",
            "NinjaOne's own workflow status, across every severity: Awaiting Approval waits on \
             a person, Approved, Not Installed on the agent. A pending patch with neither status \
             is in neither column."
                .to_string(),
        ),
        (
            "All organizations",
            "Every counted device once, so its percentages are not the average of the \
             organization rows."
                .to_string(),
        ),
        (
            "Percentages",
            "To one decimal, and never rounded up to 100%: 99.96% shows as 99.9%.".to_string(),
        ),
    ]
}

/// The bands a Within SLA bar is coloured by, as (lower bound, legend label, CSS
/// class), best first. Mirrors `rows::PCT_BANDS` (labels) and the report's colors.
pub(crate) const PCT_BANDS: [(f64, &str, &str); 3] = [
    (95.0, "95% or more", "bar-good"),
    (80.0, "80% to under 95%", "bar-warn"),
    (0.0, "under 80%", "bar-bad"),
];

/// The CSS class of the [`PCT_BANDS`] entry a percentage falls in.
pub(crate) fn pct_band_class(pct: f64) -> &'static str {
    PCT_BANDS
        .iter()
        .find(|(bound, _, _)| pct >= *bound)
        .map_or(PCT_BANDS[PCT_BANDS.len() - 1].2, |(_, _, class)| class)
}
