//! The sentences every surface prints about what a compliance figure means: the
//! headline figures, the "how these numbers are calculated" definitions, the note
//! naming organizations with no row, the first-seen line and the bar legend.
//!
//! The workbook and the report print these from here; the in-app Compliance tab
//! prints `web-rs`'s `util::compliance` mirrors. The crates share no code, so the
//! IPC fixture carries this module's output for the fixture's own result under
//! `wording`, and the frontend's `the_wording_matches_the_backend` requires its
//! mirrors to produce exactly the same strings.

use serde::Serialize;

use crate::settings::{SlaBySeverity, SlaPolicy};

use super::*;

/// A count with thousands separators — `12300` → `12,300`. Mirrors `web-rs`'s
/// `util::group_thousands`.
pub fn group_thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let len = digits.len();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// One headline figure: its label, value, an "n of N" detail and a one-line
/// definition. The Compliance tab draws these as tiles; the report prints them in
/// its header and the workbook on its About sheet.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HeadlineFigure {
    pub label: &'static str,
    pub value: String,
    pub detail: String,
    pub definition: &'static str,
}

/// The four headline figures for a result's fleet total.
///
/// A percentage over no counted device is "—", never the 100% an empty bucket
/// computes: an empty scope is not a compliant fleet.
pub fn headline_figures(
    total: &ComplianceBucket,
    devices_offline: usize,
    devices_unpatchable: usize,
) -> [HeadlineFigure; 4] {
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
        HeadlineFigure {
            label: "Within SLA",
            value: pct(total.within_sla_pct),
            detail: of(total.devices_within_sla),
            definition: "Devices with no pending patch past its severity's SLA window.",
        },
        HeadlineFigure {
            label: "Fully patched",
            value: pct(total.fully_patched_pct),
            detail: of(total.devices_fully_patched),
            definition: "Devices with no pending patch of any severity or age.",
        },
        HeadlineFigure {
            label: "Urgent pending",
            value: group_thousands(total.urgent_pending),
            detail: past,
            definition: "Critical, Important and Security patches not yet installed.",
        },
        HeadlineFigure {
            label: "Excluded devices",
            value: group_thousands(devices_offline + devices_unpatchable),
            detail: excluded,
            definition: "Offline, or not patchable by NinjaOne, so in no figure here.",
        },
    ]
}

impl FirstSeenTracking {
    /// What "first seen" means in this result: the earlier of NinjaOne's time and
    /// this app's own record, since when that record has been kept, and how many
    /// pending records it dated. `None` when no patch family is in the result.
    pub fn sentence(&self) -> Option<String> {
        let since = self.since.as_deref()?;
        let dated = match self.from_app_record {
            0 => "every pending record here is dated by NinjaOne's time".to_string(),
            n => format!(
                "{} of {} pending records are dated by this app's record, because NinjaOne's \
                 time on them is later",
                group_thousands(n),
                group_thousands(self.pending_records)
            ),
        };
        Some(format!(
            "The earlier of NinjaOne's record time and when this app first saw the patch \
             pending (this app's record began {since}); {dated}."
        ))
    }
}

/// The "How these numbers are calculated" definitions, as (term, meaning), built
/// from the result's own policy, patch families and first-seen record so they
/// describe the numbers beside them rather than today's Settings.
pub fn calculation_notes(
    policy: &SlaPolicy,
    families: PatchFamilies,
    first_seen: &FirstSeenTracking,
) -> Vec<(&'static str, String)> {
    let any_exempt = SlaBySeverity::BANDS
        .into_iter()
        .any(|s| policy.exempt.get(s));
    let no_sla = if any_exempt {
        " A severity with No SLA is never past SLA, but its patches still count as pending."
    } else {
        ""
    };
    let first_seen = first_seen.sentence().unwrap_or_else(|| {
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
                policy.describe()
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

/// How many organizations [`uncounted_orgs_note`] names before "and N more".
pub const UNCOUNTED_ORGS_LIMIT: usize = 10;

/// The line under the organization table naming the organizations with no row in
/// it — every device offline or not patchable — or `None` when there are none.
pub fn uncounted_orgs_note(orgs: &[String]) -> Option<String> {
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

/// The bands a Within SLA bar is coloured by, as (lower bound, legend label), best
/// first; a value falls in the first band whose bound it reaches.
pub const PCT_BANDS: [(f64, &str); 3] = [
    (95.0, "95% or more"),
    (80.0, "80% to under 95%"),
    (0.0, "under 80%"),
];

/// Which of [`PCT_BANDS`] a percentage falls in, as an index.
pub fn pct_band(pct: f64) -> usize {
    PCT_BANDS
        .iter()
        .position(|(bound, _)| pct >= *bound)
        .unwrap_or(PCT_BANDS.len() - 1)
}
