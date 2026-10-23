//! Live NinjaOne job status: when to ask the backend again, and what a device's
//! "job running" chip says.

use crate::types::RunningJob;

/// How often the running jobs are read while the window is visible.
pub(crate) const LIVE_JOBS_POLL_MS: f64 = 30_000.0;

/// How long to wait after a failed read. A token that cannot read `/jobs` fails
/// the same way every time, and a 30-second retry would only fill the log.
pub(crate) const LIVE_JOBS_RETRY_MS: f64 = 300_000.0;

/// The last read: when it was sent, whether it failed, and which result it was
/// scoped to (`QueryResult::generated_at`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LivePoll {
    pub at_ms: f64,
    pub failed: bool,
    pub result_key: String,
}

/// Whether to read the running jobs now. Never without a result to scope them to,
/// while the window is hidden, or while a read is in flight; at once for a result
/// the last read was not scoped to (a query just landed); otherwise on the
/// cadence, or the longer one after a failure.
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
                LIVE_JOBS_POLL_MS
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
        }
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
