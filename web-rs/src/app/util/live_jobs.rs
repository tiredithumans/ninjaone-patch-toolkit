//! Live NinjaOne job status: when to ask the backend again, and what a device's
//! "job running" chip says.

use crate::types::RunningJob;

/// How often the running jobs are read while the window is visible and NinjaOne
/// reports something running. Each empty read in a row doubles the wait, up to
/// four times this (`idle_wait_ms`).
pub(crate) const LIVE_JOBS_POLL_MS: f64 = 30_000.0;

/// The most doublings an idle streak earns: 30 s, 60 s, then 120 s.
const LIVE_JOBS_MAX_BACKOFF: u8 = 2;

/// How long to wait after a failed read. A token that cannot read `/jobs` fails
/// the same way every time, and a 30-second retry would only fill the log.
pub(crate) const LIVE_JOBS_RETRY_MS: f64 = 300_000.0;

/// The last read: when it was sent, whether it failed, which result it was
/// scoped to (`QueryResult::generated_at`), and how many reads of that result in a
/// row found nothing running (`next_empty_streak`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LivePoll {
    pub at_ms: f64,
    pub failed: bool,
    pub result_key: String,
    pub empty_streak: u8,
}

/// The wait before the next read after `empty_streak` empty reads in a row.
///
/// Not gated on a dispatch from this app: the feature exists to show jobs
/// NinjaOne runs on its own (a patch window), so it keeps looking — just less
/// often while there is nothing to see. A dispatch resets the streak.
pub(crate) fn idle_wait_ms(empty_streak: u8) -> f64 {
    LIVE_JOBS_POLL_MS * f64::from(1u8 << empty_streak.min(LIVE_JOBS_MAX_BACKOFF))
}

/// The idle streak after a read of `result_key`: one more when it found nothing
/// running, back to zero when it found something or the result is a new one. A
/// failed read (`empty` is `None`) keeps the streak; the failure wait governs.
pub(crate) fn next_empty_streak(
    last: Option<&LivePoll>,
    result_key: &str,
    empty: Option<bool>,
) -> u8 {
    let streak = last
        .filter(|p| p.result_key == result_key)
        .map_or(0, |p| p.empty_streak);
    match empty {
        Some(true) => streak.saturating_add(1),
        Some(false) => 0,
        None => streak,
    }
}

/// Whether to read the running jobs now. Never without a result to scope them to,
/// while the window is hidden, or while a read is in flight; at once for a result
/// the last read was not scoped to (a query just landed); otherwise on the
/// cadence (longer while reads keep coming back empty), or the longer one after a
/// failure.
pub(crate) fn live_jobs_due(
    last: Option<&LivePoll>,
    result_key: Option<&str>,
    now_ms: f64,
    hidden: bool,
    in_flight: bool,
) -> bool {
    let Some(key) = result_key else {
        return false;
    };
    if hidden || in_flight {
        return false;
    }
    match last {
        None => true,
        Some(p) if p.result_key != key => true,
        Some(p) => {
            let wait = if p.failed {
                LIVE_JOBS_RETRY_MS
            } else {
                idle_wait_ms(p.empty_streak)
            };
            now_ms - p.at_ms >= wait
        }
    }
}

/// The "job running" chip's tooltip for `device_id`, or `None` when nothing is
/// running there: each job's type and status, in the backend's order.
pub(crate) fn running_jobs_title(jobs: &[RunningJob], device_id: i64) -> Option<String> {
    let parts: Vec<String> = jobs
        .iter()
        .filter(|j| j.device_id == device_id)
        .map(|j| format!("{} \u{2014} {}", j.job_type, j.status))
        .collect();
    (!parts.is_empty()).then(|| format!("Running in NinjaOne: {}", parts.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poll(at_ms: f64, failed: bool, key: &str) -> LivePoll {
        LivePoll {
            at_ms,
            failed,
            result_key: key.to_string(),
            empty_streak: 0,
        }
    }

    fn idle(at_ms: f64, key: &str, empty_streak: u8) -> LivePoll {
        LivePoll {
            empty_streak,
            ..poll(at_ms, false, key)
        }
    }

    #[test]
    fn empty_reads_in_a_row_back_off_to_four_times_the_cadence() {
        assert_eq!(idle_wait_ms(0), 30_000.0);
        assert_eq!(idle_wait_ms(1), 60_000.0);
        assert_eq!(idle_wait_ms(2), 120_000.0);
        assert_eq!(idle_wait_ms(u8::MAX), 120_000.0, "capped");

        let now = 1_000_000.0;
        // One empty read: 30 s is no longer enough, 60 s is.
        let once = idle(now - LIVE_JOBS_POLL_MS, "r1", 1);
        assert!(!live_jobs_due(Some(&once), Some("r1"), now, false, false));
        let once = idle(now - 60_000.0, "r1", 1);
        assert!(live_jobs_due(Some(&once), Some("r1"), now, false, false));
        // A long idle streak waits two minutes, never more.
        let long = idle(now - 119_999.0, "r1", 9);
        assert!(!live_jobs_due(Some(&long), Some("r1"), now, false, false));
        let long = idle(now - 120_000.0, "r1", 9);
        assert!(live_jobs_due(Some(&long), Some("r1"), now, false, false));
        // A new result is still read at once, whatever the streak.
        let fresh = idle(now - 1_000.0, "r1", 9);
        assert!(live_jobs_due(Some(&fresh), Some("r2"), now, false, false));
    }

    #[test]
    fn the_idle_streak_resets_on_a_job_a_new_result_or_nothing_known() {
        let last = idle(0.0, "r1", 2);
        assert_eq!(
            next_empty_streak(None, "r1", Some(true)),
            1,
            "first empty read"
        );
        assert_eq!(next_empty_streak(Some(&last), "r1", Some(true)), 3);
        assert_eq!(
            next_empty_streak(Some(&last), "r1", Some(false)),
            0,
            "something running"
        );
        assert_eq!(
            next_empty_streak(Some(&last), "r2", Some(true)),
            1,
            "a new result starts over"
        );
        assert_eq!(
            next_empty_streak(Some(&last), "r1", None),
            2,
            "a failure neither extends nor resets it"
        );
        let maxed = idle(0.0, "r1", u8::MAX);
        assert_eq!(next_empty_streak(Some(&maxed), "r1", Some(true)), u8::MAX);
    }

    #[test]
    fn a_read_waits_for_a_result_a_visible_window_and_the_last_read() {
        let now = 1_000_000.0;
        assert!(
            !live_jobs_due(None, None, now, false, false),
            "nothing to scope to"
        );
        assert!(
            live_jobs_due(None, Some("r1"), now, false, false),
            "first read"
        );
        assert!(!live_jobs_due(None, Some("r1"), now, true, false), "hidden");
        assert!(
            !live_jobs_due(None, Some("r1"), now, false, true),
            "in flight"
        );

        let fresh = poll(now - 10_000.0, false, "r1");
        assert!(!live_jobs_due(Some(&fresh), Some("r1"), now, false, false));
        assert!(
            live_jobs_due(Some(&fresh), Some("r2"), now, false, false),
            "a new result is read at once"
        );
        let stale = poll(now - LIVE_JOBS_POLL_MS, false, "r1");
        assert!(live_jobs_due(Some(&stale), Some("r1"), now, false, false));

        let failed = poll(now - LIVE_JOBS_POLL_MS, true, "r1");
        assert!(
            !live_jobs_due(Some(&failed), Some("r1"), now, false, false),
            "a failure waits the longer retry"
        );
        let failed_long_ago = poll(now - LIVE_JOBS_RETRY_MS, true, "r1");
        assert!(live_jobs_due(
            Some(&failed_long_ago),
            Some("r1"),
            now,
            false,
            false
        ));
    }

    #[test]
    fn the_chip_names_each_running_job_on_its_device() {
        let job = |device_id: i64, job_type: &str, status: &str| RunningJob {
            uid: None,
            device_id,
            device_name: format!("srv{device_id}"),
            organization: "Contoso".into(),
            job_type: job_type.into(),
            status: status.into(),
            detail: None,
            source_name: None,
            started_at: None,
            updated_at: None,
        };
        let jobs = [
            job(1, "OS patching", "In progress"),
            job(2, "Script", "Started"),
            job(1, "System", "Starting"),
        ];
        assert_eq!(
            running_jobs_title(&jobs, 1).as_deref(),
            Some("Running in NinjaOne: OS patching \u{2014} In progress; System \u{2014} Starting")
        );
        assert_eq!(running_jobs_title(&jobs, 3), None);
    }
}
