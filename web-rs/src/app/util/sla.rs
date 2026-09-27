//! The SLA policy, the per-device backlog lists and the first-seen → installed
//! table: parsing the per-band inputs and the sentences these surfaces print.

use crate::types::{DeviceBacklogList, SlaBySeverity, SlaPolicy, TimeToInstall};

/// Upper bound for every day window, matching the backend's `MAX_WINDOW_DAYS`.
pub(crate) const MAX_SLA_DAYS: i64 = 3650;

/// Parses one per-band SLA input: blank clears the override (the band then uses
/// the default), a number is clamped into `1..=MAX_SLA_DAYS`, and anything else
/// keeps what was there. `<input type="number">` treats min/max as advisory, so
/// the clamp is the real guard — the backend rejects out-of-range values anyway.
pub(crate) fn parse_optional_days(raw: &str, current: Option<i64>) -> Option<i64> {
    let t = raw.trim();
    if t.is_empty() {
        return None;
    }
    match t.parse::<i64>() {
        Ok(v) => Some(v.clamp(1, MAX_SLA_DAYS)),
        Err(_) => current,
    }
}

fn days(n: i64) -> &'static str {
    if n == 1 { "day" } else { "days" }
}

/// One line naming the policy, e.g. `30 days (default); Critical 7 days, Important
/// 14 days`. Mirrors `settings::SlaPolicy::describe` so the in-app line reads the
/// same as the exports' — both crates are tested.
pub(crate) fn sla_policy_summary(policy: &SlaPolicy) -> String {
    let d = policy.default_days;
    let mut out = format!("{d} {} (default)", days(d));
    let overrides: Vec<String> = SlaBySeverity::BANDS
        .iter()
        .filter_map(|(label, get, _)| {
            get(&policy.by_severity).map(|n| format!("{label} {n} {}", days(n)))
        })
        .collect();
    if !overrides.is_empty() {
        out.push_str("; ");
        out.push_str(&overrides.join(", "));
    }
    out
}

/// "Showing the top 25 of 140 devices." under a capped list, `None` when the list
/// is complete.
pub(crate) fn device_list_caption(list: &DeviceBacklogList) -> Option<String> {
    (list.devices_total > list.devices.len()).then(|| {
        format!(
            "Showing the top {} of {} devices.",
            list.devices.len(),
            list.devices_total
        )
    })
}

/// Why the first-seen → installed table is empty, or `None` when it has rows.
/// Mirrors `rows::TimeToInstall::empty_reason`.
pub(crate) fn time_to_install_empty_reason(t: &TimeToInstall) -> Option<&'static str> {
    if !t.installs_queried {
        Some(
            "Select the Installed status and run a query to measure first seen \u{2192} installed.",
        )
    } else if t.overall.is_none() {
        Some("No installed record in this query carries both a first-seen and an install time.")
    } else {
        None
    }
}

/// Days to one decimal: `0.4 days`, `1.0 day`, `12.5 days`.
pub(crate) fn format_days(d: f64) -> String {
    let shown = (d * 10.0).round() / 10.0;
    let unit = if shown == 1.0 { "day" } else { "days" };
    format!("{shown:.1} {unit}")
}

/// How many installed records the table measured and how many it could not.
pub(crate) fn time_to_install_sample_note(t: &TimeToInstall) -> String {
    let measured = t.overall.as_ref().map_or(0, |o| o.samples);
    let records = |n: usize| if n == 1 { "record" } else { "records" };
    if t.excluded_records == 0 {
        format!("{measured} installed {} measured.", records(measured))
    } else {
        format!(
            "{measured} installed {} measured; {} skipped (missing a time, or installed before first seen).",
            records(measured),
            t.excluded_records
        )
    }
}

/// Printed under the table: what "first seen" is, and so how far to trust it.
pub(crate) const TIME_TO_INSTALL_NOTE: &str = "\"First seen\" is NinjaOne's record timestamp \
     (when the data was collected or updated). On an install-history record it can sit close \
     to the install itself, so these figures are indicative only.";

/// Printed under the offline list: what the data can and cannot say about it.
pub(crate) const OFFLINE_BACKLOG_NOTE: &str = "These records were collected before the device \
     went offline and may be stale; the compliance figures exclude these devices. \"Latest \
     data\" is the newest collection time on the device's records, not a last-contact time.";

/// The SLA window for one severity label: its band's override, else the default.
/// A label with no overridable band (`Unknown`, or anything unmapped) takes the
/// default, as in the backend.
pub(crate) fn sla_days_for(policy: &SlaPolicy, severity: &str) -> i64 {
    SlaBySeverity::BANDS
        .iter()
        .find(|(label, _, _)| label.eq_ignore_ascii_case(severity))
        .and_then(|(_, get, _)| get(&policy.by_severity))
        .unwrap_or(policy.default_days)
}

/// The median of an ascending slice (the mean of the two middle values for an
/// even count; 0 when empty). Mirrors `rows::install_time::median`, for the demo.
pub(crate) fn median(sorted: &[i64]) -> f64 {
    let n = sorted.len();
    match n {
        0 => 0.0,
        _ if n % 2 == 1 => sorted[n / 2] as f64,
        _ => (sorted[n / 2 - 1] as f64 + sorted[n / 2] as f64) / 2.0,
    }
}

/// Nearest-rank percentile of an ascending slice (0 when empty). Mirrors
/// `rows::install_time::nearest_rank`, for the demo.
pub(crate) fn nearest_rank(sorted: &[i64], pct: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[(pct * sorted.len()).div_ceil(100).max(1) - 1]
}
