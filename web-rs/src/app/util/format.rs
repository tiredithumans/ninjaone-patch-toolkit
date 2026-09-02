//! Display formatting: counts, labels, durations, the summary line, the
//! compliance scope note, percentages, CSS-class pickers and the aged badge.

use crate::types::{JobReport, PatchFamilies, RunRecord};

use super::super::Tab;

/// Formats a count with thousands separators (e.g. `12300` → `12,300`).
pub(crate) fn group_thousands(n: usize) -> String {
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

pub(crate) fn tab_class(active: Tab, this: Tab) -> &'static str {
    if active == this { "tab tab-on" } else { "tab" }
}

/// The count shown on a group header, which counts the *other* axis from the one
/// the rows are grouped by: a by-device group is summarised by how many patches it
/// holds, a by-patch group by how many devices it spans.
///
/// Extracted because the two are trivially invertible and the inverted form still
/// renders a plausible-looking number — the kind of mistake only an assertion
/// catches.
pub(crate) fn group_count_label(by_device: bool, rows: usize, devices: usize) -> String {
    if by_device {
        format!("{} patches", group_thousands(rows))
    } else {
        format!("{} devices", group_thousands(devices))
    }
}

/// The counts the tier-aware results summary needs — all already on `QueryResult`,
/// so the summary can describe whichever tab is active without extra backend data.
pub(crate) struct SummaryCounts {
    pub rows_total: usize,
    pub devices_total: usize,
    pub failures: usize,
    /// Distinct devices carrying at least one of those failures.
    pub failing_devices: usize,
    pub orgs: usize,
    pub reboot: usize,
}

/// Builds the results-summary line for the active tab. The old shared line read
/// "{rows} patch rows …" across every tab, which is the Patches detail-row count —
/// misleading on Compliance/Reboot, whose scope is devices, not patch rows.
pub(crate) fn summary_line(tab: Tab, c: &SummaryCounts, generated_at: &str) -> String {
    let head = match tab {
        Tab::Patches => format!(
            "{} patch rows across {} devices",
            group_thousands(c.rows_total),
            group_thousands(c.devices_total),
        ),
        // Against the devices that actually failed, not the fleet. "12 failing
        // patches across 4,000 devices" put the fleet size where the reader expects
        // the blast radius, making a contained problem look fleet-wide.
        Tab::Failures => format!(
            "{} failing patches on {} devices",
            group_thousands(c.failures),
            group_thousands(c.failing_devices),
        ),
        Tab::Compliance => format!(
            "{} organizations \u{00b7} {} devices",
            group_thousands(c.orgs),
            group_thousands(c.devices_total),
        ),
        Tab::Reboot => format!(
            "{} of {} devices need reboot",
            group_thousands(c.reboot),
            group_thousands(c.devices_total),
        ),
        // Jobs describes dispatched actions, not the query result, so the query's
        // counts and generation time would be actively misleading here. Trend is the
        // same: it reads the run-history file, which spans many queries and has no
        // single generation time.
        Tab::Jobs => return "Dispatched actions".to_string(),
        Tab::Trend => return "Fleet health over time".to_string(),
    };
    format!("{head} \u{00b7} generated {generated_at}")
}

/// Fleet-health tabs (Compliance, Needs Reboot) reflect the device scope only and
/// ignore the patch filters; Filtered-results tabs (Patches, Failures) honor them.
/// Jobs and Trend are neither — they show dispatch history and run history, not the
/// output of the current query.
pub(crate) fn is_fleet_tab(tab: Tab) -> bool {
    matches!(tab, Tab::Compliance | Tab::Reboot)
}

/// Short mode label for a dispatched job. A dry run applies — and reboots —
/// nothing, so it wins over the reboot indication.
pub(crate) fn job_mode_label(job: &JobReport) -> &'static str {
    if job.dry_run {
        "Dry run"
    } else if job.kind.can_reboot() {
        "Live + reboot"
    } else {
        "Live"
    }
}

/// Renders an elapsed-seconds count as `1m 20s` / `45s`, or blank while running.
pub(crate) fn format_duration(seconds: Option<i64>) -> String {
    match seconds {
        None => String::new(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) => format!("{}m {}s", s / 60, s % 60),
    }
}

/// One sentence stating exactly which devices and which patches the compliance
/// numbers describe.
///
/// Mirrors `src-tauri/src/rows.rs::compliance_scope_note`, which the workbook and the
/// HTML report print — the two crates share no code, so the wording lives twice and
/// the tests on both sides pin it. Two things are invisible in a bare percentage and
/// both change what it means: offline devices are excluded from the rollups entirely,
/// and only the patch families the query fetched are in them.
pub(crate) fn compliance_scope_note(
    devices_offline: usize,
    devices_unpatchable: usize,
    families: PatchFamilies,
) -> String {
    let excluded = excluded_clause(devices_offline, devices_unpatchable);
    let families = if families.is_complete() {
        String::new()
    } else {
        format!(", and counts {}", families.label())
    };
    format!("Compliance covers online Windows, macOS and Linux devices only{excluded}{families}.")
}

/// Mirrors `rows::excluded_clause`: the parenthetical naming the devices the rollups
/// left out, so `devices_total − offline − non-patchable` reconciles with the table.
fn excluded_clause(offline: usize, unpatchable: usize) -> String {
    let devices = |n: usize| if n == 1 { "device" } else { "devices" };
    match (offline, unpatchable) {
        (0, 0) => String::new(),
        (n, 0) => format!(" ({n} offline {} excluded)", devices(n)),
        (0, m) => format!(" ({m} non-patchable {} excluded)", devices(m)),
        (n, m) => format!(
            " ({n} offline and {m} non-patchable {} excluded)",
            devices(n + m)
        ),
    }
}

/// Formats a compliance percentage at zero decimals **without ever rounding up to
/// 100%**.
///
/// Mirrors `src-tauri/src/rows.rs::format_pct`; the two crates share no code, so the
/// rule is written twice on purpose. Plain `{:.0}%` renders anything from 99.5% up as
/// `100%`, which is the one rounding error here that changes what an operator does:
/// a compliance report claiming a clean fleet stops the work. Values below 100 cap at
/// 99%.
pub(crate) fn format_pct(pct: f64) -> String {
    let shown = if pct >= 100.0 {
        100.0
    } else {
        pct.round().min(99.0)
    };
    format!("{shown:.0}%")
}

pub(crate) fn sev_class(sev: &str) -> &'static str {
    match sev {
        "Critical" => "sev sev-critical",
        "Important" => "sev sev-important",
        "Security" => "sev sev-security",
        "Moderate" => "sev sev-moderate",
        "Recommended" => "sev sev-recommended",
        "Low" => "sev sev-low",
        "Optional" => "sev sev-optional",
        "Unknown" => "sev sev-unknown",
        // Anything the backend's `Severity::label()` doesn't produce. Distinct from
        // `sev-unknown`, which is a severity NinjaOne *did* send and `from_raw`
        // couldn't map — that distinction is the difference between "low priority"
        // and "we don't know what this is", and both used to render identically.
        _ => "sev sev-none",
    }
}

pub(crate) fn status_class(status: &str) -> &'static str {
    match status {
        "INSTALLED" => "stat stat-installed",
        "APPROVED" => "stat stat-approved",
        "PENDING" => "stat stat-pending",
        "REJECTED" => "stat stat-rejected",
        "FAILED" => "stat stat-failed",
        _ => "stat",
    }
}

/// Presentation for an "Aged (past SLA)" table cell as (CSS class, label, title).
/// An aged backlog gets a ⚠ prefix so it reads without relying on color.
pub(crate) fn aged_badge(aged: usize) -> (&'static str, String, &'static str) {
    if aged > 0 {
        (
            "sev-critical",
            format!("⚠ {aged}"),
            "Past SLA — needs attention",
        )
    } else {
        ("", aged.to_string(), "")
    }
}

/// The runs that belong on one trend line, newest last.
///
/// History accumulates every completed query, and those queries did not all measure
/// the same thing: an OS-only run against a filtered scope is a different series
/// than a whole-fleet ALL run. Charting them together would read as the backlog
/// halving overnight when all that changed was the Type chip. So the newest record
/// defines the series and only records comparable with it are kept.
///
/// Returns at most `limit` records, keeping the most recent.
pub(crate) fn trend_series(history: &[RunRecord], limit: usize) -> Vec<RunRecord> {
    let Some(newest) = history.last() else {
        return Vec::new();
    };
    let mut series: Vec<RunRecord> = history
        .iter()
        .filter(|r| r.comparable_with(newest))
        .cloned()
        .collect();
    if series.len() > limit {
        series.drain(..series.len() - limit);
    }
    series
}

/// `(x, y)` points in a 0..=1 box for a sparkline over `values`, oldest first.
///
/// Y is inverted (0 is the top) so callers can use it as an SVG coordinate directly.
/// A flat series is drawn down the middle rather than divided by a zero range.
pub(crate) fn sparkline_points(values: &[f64]) -> Vec<(f64, f64)> {
    if values.is_empty() {
        return Vec::new();
    }
    if values.len() == 1 {
        return vec![(0.5, 0.5)];
    }
    let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let range = max - min;
    values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = i as f64 / (values.len() - 1) as f64;
            let y = if range <= f64::EPSILON {
                0.5
            } else {
                1.0 - (v - min) / range
            };
            (x, y)
        })
        .collect()
}
