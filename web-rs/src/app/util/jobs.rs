//! The Jobs list: merging live rows into it, and what "still in flight" means for
//! the controls that must wait for dispatched work to settle.

use crate::types::JobReport;

/// Upserts `incoming` into `jobs` by job id: a known id is replaced in place (the
/// row arrives already advanced), an unknown one is appended.
///
/// Both writers go through this — the `action:progress` listener and the
/// `run_action` response — because they race. The poller emits a batch's rows over
/// the event while the dispatch response is still in flight, so an append-only
/// response handler listed every job twice on the Jobs tab.
pub(crate) fn merge_jobs(jobs: &mut Vec<JobReport>, incoming: impl IntoIterator<Item = JobReport>) {
    for job in incoming {
        match jobs.iter_mut().find(|j| j.id == job.id) {
            Some(slot) => *slot = job,
            None => jobs.push(job),
        }
    }
}

/// How many jobs have not reached a terminal state (queued, running, or still being
/// resolved after an ambiguous dispatch).
pub(crate) fn jobs_in_flight(jobs: &[JobReport]) -> usize {
    jobs.iter().filter(|j| !j.state.is_terminal()).count()
}

/// Why "Update & restart" must wait, or `None` when it may run.
///
/// The install relaunches the app, which kills the backend's job poller mid-flight:
/// a dispatch in progress would lose the devices it had not reached yet, and the
/// jobs already sent would never be resolved on this session's Jobs tab.
pub(crate) fn update_blocked_reason(dispatching: bool, in_flight: usize) -> Option<String> {
    if dispatching {
        return Some("An action is being dispatched — update once it has gone out.".to_string());
    }
    if in_flight > 0 {
        return Some(format!(
            "{in_flight} dispatched job(s) are still running — update once they finish, or \
             their results will not be tracked."
        ));
    }
    None
}
