//! What each column of a client-sorted table sorts by: the tables whose rows the
//! summary ships whole (Failures, the Compliance tables, Jobs). One function per
//! table, indexed like its header array, beside [`sort_by_column`](super::sort_by_column)
//! which does the sorting. A column a function answers `Missing` for on every row
//! is listed in the table's `*_UNSORTABLE`, and its header is plain text.

use super::{SortValue, sev_ordinal};
use crate::types::{
    ComplianceBucket, DeviceBacklog, FailureGroup, JobReport, JobState, OsCompliance, RunningJob,
    StuckDevice,
};

/// The Failures table's headers, as `rows::FailureGroup::COLUMNS` spells them.
pub(crate) const FAILURE_HEADERS: [&str; 7] = [
    "Severity",
    "Patch Type",
    "KB",
    "Patch",
    "Affected Devices",
    "Latest Failure",
    "Devices",
];

/// The Devices cell is a list of names, not a value to order by.
pub(crate) const FAILURE_UNSORTABLE: [usize; 1] = [6];

pub(crate) fn failure_sort_value(f: &FailureGroup, col: usize) -> SortValue<'_> {
    match col {
        // Most urgent first when ascending, as the Patches table sorts severity.
        0 => SortValue::Number(f64::from(sev_ordinal(&f.severity))),
        1 => SortValue::text(Some(&f.patch_type)),
        2 => SortValue::text(f.kb.as_deref()),
        3 => SortValue::text(Some(&f.name)),
        4 => SortValue::count(f.affected_devices),
        5 => SortValue::text(f.latest_failure.as_deref()),
        _ => SortValue::Missing,
    }
}

/// One row of a compliance rollup table, independent of the grouping key. The two
/// bucket types stay distinct hand-maintained IPC mirrors (`types.rs`); they
/// converge here only for rendering and sorting.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ComplianceRow {
    pub label: String,
    pub devices_total: usize,
    pub devices_within_sla: usize,
    pub within_sla_pct: f64,
    pub devices_fully_patched: usize,
    pub fully_patched_pct: f64,
    pub urgent_pending: usize,
    pub urgent_past_sla: usize,
    pub awaiting_approval: usize,
    pub approved_not_installed: usize,
}

impl From<&ComplianceBucket> for ComplianceRow {
    fn from(b: &ComplianceBucket) -> Self {
        Self {
            label: b.organization.clone(),
            devices_total: b.devices_total,
            devices_within_sla: b.devices_within_sla,
            within_sla_pct: b.within_sla_pct,
            devices_fully_patched: b.devices_fully_patched,
            fully_patched_pct: b.fully_patched_pct,
            urgent_pending: b.urgent_pending,
            urgent_past_sla: b.urgent_past_sla,
            awaiting_approval: b.awaiting_approval,
            approved_not_installed: b.approved_not_installed,
        }
    }
}

impl From<&OsCompliance> for ComplianceRow {
    fn from(b: &OsCompliance) -> Self {
        Self {
            label: b.os.clone(),
            devices_total: b.devices_total,
            devices_within_sla: b.devices_within_sla,
            within_sla_pct: b.within_sla_pct,
            devices_fully_patched: b.devices_fully_patched,
            fully_patched_pct: b.fully_patched_pct,
            urgent_pending: b.urgent_pending,
            urgent_past_sla: b.urgent_past_sla,
            awaiting_approval: b.awaiting_approval,
            approved_not_installed: b.approved_not_installed,
        }
    }
}

/// Both compliance rollups (`COMPLIANCE_ORG_HEADERS`, `COMPLIANCE_OS_HEADERS`):
/// the grouping label, then nine figures.
pub(crate) fn compliance_sort_value(r: &ComplianceRow, col: usize) -> SortValue<'_> {
    match col {
        0 => SortValue::text(Some(&r.label)),
        1 => SortValue::count(r.devices_total),
        2 => SortValue::count(r.devices_within_sla),
        3 => SortValue::Number(r.within_sla_pct),
        4 => SortValue::count(r.devices_fully_patched),
        5 => SortValue::Number(r.fully_patched_pct),
        6 => SortValue::count(r.urgent_pending),
        7 => SortValue::count(r.urgent_past_sla),
        8 => SortValue::count(r.awaiting_approval),
        9 => SortValue::count(r.approved_not_installed),
        _ => SortValue::Missing,
    }
}

/// "Pending by Severity" is a breakdown, not one value.
pub(crate) const WORST_UNSORTABLE: [usize; 1] = [6];
pub(crate) const OFFLINE_UNSORTABLE: [usize; 1] = [7];

/// The worst-devices list (`WORST_HEADERS`).
pub(crate) fn worst_sort_value(d: &DeviceBacklog, col: usize) -> SortValue<'_> {
    match col {
        0 => SortValue::text(Some(&d.organization)),
        1 => SortValue::text(Some(&d.device_name)),
        2 => SortValue::text(d.os_name.as_deref()),
        3 => SortValue::count(d.past_sla),
        4 => SortValue::count(d.pending_total),
        5 => SortValue::text(d.oldest_first_seen.as_deref()),
        _ => SortValue::Missing,
    }
}

/// The offline-backlog list (`OFFLINE_HEADERS`).
pub(crate) fn offline_sort_value(d: &DeviceBacklog, col: usize) -> SortValue<'_> {
    match col {
        0 => SortValue::text(Some(&d.organization)),
        1 => SortValue::text(Some(&d.device_name)),
        2 => SortValue::text(d.os_name.as_deref()),
        3 => SortValue::text(d.latest_collected.as_deref()),
        4 => SortValue::text(d.last_contact.as_deref()),
        5 => SortValue::count(d.pending_total),
        6 => SortValue::count(d.past_sla),
        _ => SortValue::Missing,
    }
}

/// The stuck-approvals table (`STUCK_HEADERS`).
pub(crate) fn stuck_sort_value(d: &StuckDevice, col: usize) -> SortValue<'_> {
    match col {
        0 => SortValue::text(Some(&d.device_name)),
        1 => SortValue::text(Some(&d.organization)),
        2 => SortValue::count(d.patches),
        3 => SortValue::text(d.oldest_first_seen.as_deref()),
        _ => SortValue::Missing,
    }
}

/// The session Jobs table's headers.
pub(crate) const JOB_HEADERS: [&str; 9] = [
    "Device",
    "Organization",
    "Action",
    "Mode",
    "Status",
    "Exit",
    "Dispatched",
    "Duration",
    "Retry",
];

/// Retry is a button or a note about one.
pub(crate) const JOB_UNSORTABLE: [usize; 1] = [8];

/// Where a job is in its life, for the Status column: what is still in flight
/// first, then what needs a look, then what finished cleanly.
fn job_state_rank(state: &JobState) -> u8 {
    match state {
        JobState::Queued => 0,
        JobState::Running => 1,
        JobState::Unknown(_) => 2,
        JobState::Failed(_) => 3,
        JobState::TimedOut => 4,
        JobState::Skipped(_) => 5,
        JobState::Completed => 6,
    }
}

pub(crate) fn job_sort_value(j: &JobReport, col: usize) -> SortValue<'_> {
    match col {
        0 => SortValue::text(Some(&j.device_name)),
        1 => SortValue::text(Some(&j.organization)),
        2 => SortValue::text(Some(&j.detail)),
        3 => SortValue::Text(super::job_mode_label(j)),
        4 => SortValue::Number(f64::from(job_state_rank(&j.state))),
        5 => j
            .exit_code
            .map_or(SortValue::Missing, |c| SortValue::Number(f64::from(c))),
        6 => SortValue::text(Some(&j.dispatched_at)),
        7 => j
            .duration_seconds
            .map_or(SortValue::Missing, |s| SortValue::Number(s as f64)),
        _ => SortValue::Missing,
    }
}

/// The "Running in NinjaOne" table's headers on the Jobs tab.
pub(crate) const RUNNING_JOB_HEADERS: [&str; 8] = [
    "Device",
    "Organization",
    "Job",
    "Status",
    "Detail",
    "Started by",
    "Started",
    "Updated",
];

pub(crate) fn running_job_sort_value(j: &RunningJob, col: usize) -> SortValue<'_> {
    match col {
        0 => SortValue::text(Some(&j.device_name)),
        1 => SortValue::text(Some(&j.organization)),
        2 => SortValue::text(Some(&j.job_type)),
        3 => SortValue::text(Some(&j.status)),
        4 => SortValue::text(j.detail.as_deref()),
        5 => SortValue::text(j.source_name.as_deref()),
        6 => SortValue::text(j.started_at.as_deref()),
        7 => SortValue::text(j.updated_at.as_deref()),
        _ => SortValue::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{
        COMPLIANCE_ORG_HEADERS, COMPLIANCE_OS_HEADERS, ColumnSort, OFFLINE_HEADERS, STUCK_HEADERS,
        WORST_HEADERS, column_aria_sort, column_sort_glyph, next_column_sort, sort_by_column,
    };
    use super::*;
    use crate::types::{QueryResult, RunningJobs};

    fn fixture() -> serde_json::Value {
        serde_json::from_str(include_str!("../../../tests/backend-ipc.json")).expect("fixture")
    }

    /// The backend's own filled samples: every field the tables sort by is set.
    fn summary() -> QueryResult {
        serde_json::from_value(fixture()["commands"]["query_patches"].clone()).expect("summary")
    }

    fn asc(col: usize) -> Option<ColumnSort> {
        Some(ColumnSort { col, desc: false })
    }

    fn desc(col: usize) -> Option<ColumnSort> {
        Some(ColumnSort { col, desc: true })
    }

    /// A header click cycles ascending → descending → off, as the Patches table's
    /// does, and the header says which it is.
    #[test]
    fn a_header_click_cycles_ascending_descending_off() {
        let first = next_column_sort(None, 2);
        assert_eq!(first, asc(2));
        assert_eq!(next_column_sort(first, 2), desc(2));
        assert_eq!(next_column_sort(desc(2), 2), None);
        // Another column starts over, ascending.
        assert_eq!(next_column_sort(desc(2), 5), asc(5));

        assert_eq!(column_aria_sort(asc(2), 2), "ascending");
        assert_eq!(column_aria_sort(desc(2), 2), "descending");
        assert_eq!(column_aria_sort(asc(2), 3), "none");
        assert_eq!(column_aria_sort(None, 2), "none");
        assert_eq!(column_sort_glyph(asc(2), 2), " ▲");
        assert_eq!(column_sort_glyph(desc(2), 2), " ▼");
        assert_eq!(column_sort_glyph(asc(2), 3), "");
    }

    /// Numbers sort as numbers, text without regard to case, a blank last in
    /// either direction, and rows that tie stay in the order they arrived in.
    #[test]
    fn a_column_sorts_by_value_with_blanks_last_and_ties_in_place() {
        // (name, count, arrival order)
        type Row = (Option<&'static str>, usize, u8);
        let rows = || -> Vec<Row> {
            vec![
                (Some("beta"), 10, 0),
                (None, 9, 1),
                (Some("Alpha"), 100, 2),
                (Some("alpha"), 9, 3),
            ]
        };
        fn value(r: &Row, col: usize) -> SortValue<'_> {
            match col {
                0 => SortValue::text(r.0),
                _ => SortValue::count(r.1),
            }
        }
        let order = |sort| {
            let mut rows = rows();
            sort_by_column(&mut rows, sort, value);
            rows.iter().map(|r| r.2).collect::<Vec<_>>()
        };
        assert_eq!(order(None), [0, 1, 2, 3], "no sort is the backend's order");
        // "Alpha" and "alpha" tie and keep their arrival order; the blank is last.
        assert_eq!(order(asc(0)), [2, 3, 0, 1]);
        assert_eq!(order(desc(0)), [0, 2, 3, 1], "still last when descending");
        // 9 before 10 before 100 — not "10" < "100" < "9".
        assert_eq!(order(asc(1)), [1, 3, 0, 2]);
        assert_eq!(order(desc(1)), [2, 0, 1, 3]);
    }

    /// Every header either sorts by a value or is listed as unsortable: a column
    /// added to a table without a sort value would otherwise be a button that
    /// does nothing.
    #[test]
    fn every_header_sorts_or_is_listed_as_unsortable() {
        fn check<T>(
            table: &str,
            headers: usize,
            unsortable: &[usize],
            row: &T,
            value: impl for<'a> Fn(&'a T, usize) -> SortValue<'a>,
        ) {
            for col in 0..headers {
                assert_eq!(
                    value(row, col) == SortValue::Missing,
                    unsortable.contains(&col),
                    "{table}, column {col}"
                );
            }
            assert_eq!(
                value(row, headers),
                SortValue::Missing,
                "{table}: past the end"
            );
        }
        let r = summary();
        check(
            "failures",
            FAILURE_HEADERS.len(),
            &FAILURE_UNSORTABLE,
            &r.failures[0],
            failure_sort_value,
        );
        let org = ComplianceRow::from(&r.compliance[0]);
        check(
            "organizations",
            COMPLIANCE_ORG_HEADERS.len(),
            &[],
            &org,
            compliance_sort_value,
        );
        let os = ComplianceRow::from(&r.compliance_by_os[0]);
        check(
            "by OS",
            COMPLIANCE_OS_HEADERS.len(),
            &[],
            &os,
            compliance_sort_value,
        );
        check(
            "worst devices",
            WORST_HEADERS.len(),
            &WORST_UNSORTABLE,
            &r.worst_devices.devices[0],
            worst_sort_value,
        );
        check(
            "offline",
            OFFLINE_HEADERS.len(),
            &OFFLINE_UNSORTABLE,
            &r.offline_backlog.devices[0],
            offline_sort_value,
        );
        check(
            "stuck approvals",
            STUCK_HEADERS.len(),
            &[],
            &r.approvals.stuck_devices[0],
            stuck_sort_value,
        );
        let jobs: Vec<JobReport> =
            serde_json::from_value(fixture()["commands"]["list_jobs"].clone()).expect("jobs");
        let settled = jobs
            .iter()
            .find(|j| j.exit_code.is_some() && j.duration_seconds.is_some())
            .expect("a job with an exit code and a duration");
        check(
            "jobs",
            JOB_HEADERS.len(),
            &JOB_UNSORTABLE,
            settled,
            job_sort_value,
        );
        let running: RunningJobs =
            serde_json::from_value(fixture()["commands"]["list_running_jobs"].clone())
                .expect("running jobs");
        check(
            "running jobs",
            RUNNING_JOB_HEADERS.len(),
            &[],
            &running.jobs[0],
            running_job_sort_value,
        );
    }

    /// Severity sorts by urgency, not by its name: ascending puts Critical first,
    /// as the Patches table does.
    #[test]
    fn failures_sort_by_urgency_and_by_how_many_devices() {
        let base = summary().failures[0].clone();
        let failure = |severity: &str, affected: usize, kb: Option<&str>| FailureGroup {
            severity: severity.into(),
            affected_devices: affected,
            kb: kb.map(Into::into),
            ..base.clone()
        };
        let mut rows = vec![
            failure("Low", 12, Some("KB2")),
            failure("Critical", 3, None),
            failure("Important", 100, Some("KB1")),
        ];
        let severities =
            |rows: &[FailureGroup]| rows.iter().map(|f| f.severity.clone()).collect::<Vec<_>>();
        sort_by_column(&mut rows, asc(0), failure_sort_value);
        assert_eq!(severities(&rows), ["Critical", "Important", "Low"]);
        sort_by_column(&mut rows, desc(4), failure_sort_value);
        assert_eq!(severities(&rows), ["Important", "Low", "Critical"]);
        // The patch with no KB (third-party) is last, not first.
        sort_by_column(&mut rows, asc(2), failure_sort_value);
        assert_eq!(severities(&rows), ["Important", "Low", "Critical"]);
    }

    /// The Status column orders jobs by where they are in their life — in flight,
    /// then the ones that need a look, then done — not by the label's spelling
    /// (which would file "Completed" before "Failed").
    #[test]
    fn jobs_sort_by_where_they_are_in_their_life() {
        let ranks: Vec<u8> = [
            JobState::Queued,
            JobState::Running,
            JobState::Unknown(String::new()),
            JobState::Failed(String::new()),
            JobState::TimedOut,
            JobState::Skipped(String::new()),
            JobState::Completed,
        ]
        .iter()
        .map(job_state_rank)
        .collect();
        assert!(ranks.is_sorted(), "{ranks:?}");
        // Every state has its own place.
        let mut all: Vec<u8> = JobState::ALL.iter().map(job_state_rank).collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), JobState::ALL.len());
    }
}
