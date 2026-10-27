//! The device drill-down's presentation: the fact list, the rollup verdict and
//! the capped-rows note. Pure, so the wording is host-tested rather than living in
//! the dialog component.

use crate::types::{DeviceSummary, RollupScope};

/// What the drill-down says about a device's place in the fleet-health rollups.
/// Mirrors `rows::RollupScope::label` (the workbook's Devices sheet), plus the
/// consequence, since here it explains why the counts below are or are not shown.
pub(crate) fn rollup_scope_note(scope: RollupScope) -> Option<&'static str> {
    match scope {
        RollupScope::Included => None,
        RollupScope::Offline => Some(
            "Excluded from compliance: the device is offline, so its patch records may be \
             out of date — its counts are left out rather than read as current.",
        ),
        RollupScope::NonPatchable => Some(
            "Excluded from compliance: NinjaOne patch management does not cover this \
             device class, so it reports no patch records.",
        ),
    }
}

/// The device facts as (label, value) pairs, in display order. Absent values read
/// as an em dash rather than disappearing, so the list keeps its shape from one
/// device to the next.
pub(crate) fn device_facts(d: &DeviceSummary) -> Vec<(&'static str, String)> {
    let or_dash = |v: &Option<String>| {
        v.as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or("—")
            .to_string()
    };
    vec![
        ("Organization", d.organization.clone()),
        ("Location", or_dash(&d.location)),
        ("Device Role", or_dash(&d.device_role)),
        ("OS", or_dash(&d.os_name)),
        (
            "Status",
            if d.offline { "Offline" } else { "Online" }.to_string(),
        ),
        ("Last contact", or_dash(&d.last_contact)),
        ("Needs reboot", needs_reboot_label(d)),
    ]
}

/// "1 critical, 3 high, 7 medium, 2 low" from NinjaOne's Vulnerability Management,
/// or `None` when it holds no counts for the device.
pub(crate) fn vulnerability_label(d: &DeviceSummary) -> Option<String> {
    d.vulnerabilities.map(|v| {
        format!(
            "{} critical, {} high, {} medium, {} low",
            v.critical, v.high, v.medium, v.low
        )
    })
}

/// "Yes — <NinjaOne's reason>" when the device reports why, so the drill-down says
/// what is waiting on the reboot rather than only that something is.
fn needs_reboot_label(d: &DeviceSummary) -> String {
    match (d.needs_reboot, d.reboot_reason.as_deref().map(str::trim)) {
        (true, Some(reason)) if !reason.is_empty() => format!("Yes \u{2014} {reason}"),
        (true, _) => "Yes".to_string(),
        (false, _) => "No".to_string(),
    }
}

/// Whether a counted device is Within SLA — "Yes", or "No — 3 pending past SLA" —
/// or `None` for a device outside the rollups, which shows its exclusion instead.
pub(crate) fn within_sla_label(d: &DeviceSummary) -> Option<String> {
    (d.rollup_scope == RollupScope::Included).then(|| match d.past_sla {
        0 => "Yes".to_string(),
        n => format!("No \u{2014} {n} pending past SLA"),
    })
}

/// The failed-installs figure, which is unknown — not zero — unless the query
/// asked for the Failed status.
pub(crate) fn failed_installs_label(failed: Option<usize>) -> String {
    match failed {
        Some(n) => n.to_string(),
        None => "— (query the Failed status to count)".to_string(),
    }
}

/// Says so when the backend capped the device's rows, so a partial list is never
/// read as the whole.
pub(crate) fn device_rows_note(shown: usize, total: usize) -> Option<String> {
    (total > shown).then(|| {
        format!(
            "Showing the first {} of {} rows — sorting applies to these.",
            super::group_thousands(shown),
            super::group_thousands(total),
        )
    })
}
