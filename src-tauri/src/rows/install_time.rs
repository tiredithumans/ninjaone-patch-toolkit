//! First seen → installed: how long installed patches had been known before they
//! went on, by organization and by severity band.

use std::collections::HashMap;

use serde::Serialize;

use crate::model::PatchRow;

use super::*;

const SECS_PER_DAY: f64 = 86_400.0;

/// One group's latency distribution, in days.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallLatency {
    /// `Overall`, `Organization` or `Severity` — which breakdown this row is in.
    pub group: &'static str,
    /// The organization name or severity label (`All installs` for the overall row).
    pub label: String,
    /// Install records measured.
    pub samples: usize,
    pub median_days: f64,
    /// Nearest-rank 90th percentile.
    pub p90_days: f64,
}

/// The first-seen → installed aggregate.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeToInstall {
    /// Whether the query asked for the Installed status at all. Without it there are
    /// no install records to measure, and the surfaces say so rather than showing an
    /// empty table that reads as "nothing was installed".
    pub installs_queried: bool,
    pub overall: Option<InstallLatency>,
    /// Sorted by organization name.
    pub by_organization: Vec<InstallLatency>,
    /// Most urgent band first.
    pub by_severity: Vec<InstallLatency>,
    /// Installed records seen, and how many of them could not be measured (missing
    /// either time, or installed before first seen).
    pub installed_records: usize,
    pub excluded_records: usize,
}

/// Measures every INSTALLED detail row that carries both times with the install at
/// or after first seen.
///
/// Built from the detail rows, like the failures rollup, so it follows the patch
/// facets (status, severity, search, the first-seen window and the install
/// lookback) as well as the device scope — it describes the installs the operator
/// asked to see. An install on a device that has since gone offline still counts:
/// the install happened, and this is not a fleet-health rollup over the current
/// feed.
pub fn build_time_to_install(rows: &[PatchRow], installs_queried: bool) -> TimeToInstall {
    let mut all: Vec<i64> = Vec::new();
    let mut by_org: HashMap<&str, Vec<i64>> = HashMap::new();
    // Keyed by label with the rank alongside, so the output can order by urgency.
    let mut by_sev: HashMap<&'static str, (u8, Vec<i64>)> = HashMap::new();
    let mut installed_records = 0;
    for r in rows.iter().filter(|r| &*r.status == "INSTALLED") {
        installed_records += 1;
        let (Some(seen), Some(installed)) = (r.first_seen_ts, r.installed_ts) else {
            continue;
        };
        if installed < seen {
            continue;
        }
        let secs = installed - seen;
        all.push(secs);
        by_org.entry(&r.organization).or_default().push(secs);
        by_sev
            .entry(r.severity)
            .or_insert_with(|| (r.severity_rank, Vec::new()))
            .1
            .push(secs);
    }
    let excluded_records = installed_records - all.len();

    let mut by_organization: Vec<InstallLatency> = by_org
        .into_iter()
        .map(|(org, secs)| latency("Organization", org.to_string(), secs))
        .collect();
    by_organization.sort_by(|a, b| cmp_ci(&a.label, &b.label));

    let mut by_severity: Vec<(u8, InstallLatency)> = by_sev
        .into_iter()
        .map(|(label, (rank, secs))| (rank, latency("Severity", label.to_string(), secs)))
        .collect();
    by_severity.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));

    TimeToInstall {
        installs_queried,
        overall: (!all.is_empty()).then(|| latency("Overall", "All installs".into(), all)),
        by_organization,
        by_severity: by_severity.into_iter().map(|(_, l)| l).collect(),
        installed_records,
        excluded_records,
    }
}

fn latency(group: &'static str, label: String, mut secs: Vec<i64>) -> InstallLatency {
    secs.sort_unstable();
    InstallLatency {
        group,
        label,
        samples: secs.len(),
        median_days: median(&secs) / SECS_PER_DAY,
        p90_days: nearest_rank(&secs, 90) as f64 / SECS_PER_DAY,
    }
}

/// The median of an ascending, non-empty slice: the middle value, or the mean of
/// the two middle values for an even count.
pub(super) fn median(sorted: &[i64]) -> f64 {
    let n = sorted.len();
    if n == 0 {
        return 0.0;
    }
    if n % 2 == 1 {
        sorted[n / 2] as f64
    } else {
        (sorted[n / 2 - 1] as f64 + sorted[n / 2] as f64) / 2.0
    }
}

/// Nearest-rank percentile of an ascending, non-empty slice: the smallest value
/// with at least `pct`% of the sample at or below it. No interpolation, so it is
/// always a value that was actually observed.
pub(super) fn nearest_rank(sorted: &[i64], pct: usize) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (pct * sorted.len()).div_ceil(100).max(1);
    sorted[rank - 1]
}

impl TimeToInstall {
    /// Every row the tables print, overall first — one table for the workbook
    /// sheet, where a `Breakdown` column says which group each row belongs to.
    pub fn table_rows(&self) -> Vec<&InstallLatency> {
        self.overall
            .iter()
            .chain(&self.by_organization)
            .chain(&self.by_severity)
            .collect()
    }

    /// Why there is nothing to show, or `None` when there is. Mirrored in the
    /// frontend `util` module.
    pub fn empty_reason(&self) -> Option<&'static str> {
        if !self.installs_queried {
            Some(
                "Select the Installed status and run a query to measure first seen \u{2192} installed.",
            )
        } else if self.overall.is_none() {
            Some("No installed record in this query carries both a first-seen and an install time.")
        } else {
            None
        }
    }
}

/// Rounds days to one decimal, so the workbook and the report agree on precision.
fn days_cell(days: f64) -> TableCell {
    TableCell::Number((days * 10.0).round() / 10.0)
}

impl InstallLatency {
    pub const COLUMNS: [TableColumn<InstallLatency>; 5] = [
        ("Breakdown", |l| TableCell::text(l.group)),
        ("Group", |l| TableCell::text(&l.label)),
        ("Installs Measured", |l| TableCell::Count(l.samples)),
        ("First Seen \u{2192} Installed (Median Days)", |l| {
            days_cell(l.median_days)
        }),
        ("90th Percentile (Days)", |l| days_cell(l.p90_days)),
    ];
}

/// Printed beside the figures on every surface: `timestamp` is a collection time,
/// so on an install-history record it can sit close to the install itself.
pub const TIME_TO_INSTALL_NOTE: &str = "First seen \u{2192} installed, over installed records \
     carrying both times with the install no earlier than first seen. \"First seen\" is \
     NinjaOne's record timestamp (when the data was collected or updated), which on an \
     install-history record can sit close to the install itself \u{2014} indicative only.";
