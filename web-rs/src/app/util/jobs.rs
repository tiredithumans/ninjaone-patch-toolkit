//! The Jobs list: merging live rows into it, rebuilding a failed job for a retry,
//! and what "still in flight" means for the controls that must wait for dispatched
//! work to settle.

use std::collections::BTreeMap;

use crate::types::{ActionKind, ActionRequest, JobReport, JobRequest, JobState};

/// Upserts `incoming` into `jobs` by job id: a known id is replaced in place (the
/// row arrives already advanced), an unknown one is appended — except that a row
/// already in a terminal state is never replaced by a non-terminal one.
///
/// Both writers go through this — the `action:progress` listener and the
/// `run_action` response — because they race. Dispatch emits a batch's rows over
/// the event while the response is still in flight, so an append-only response
/// handler listed every job twice on the Jobs tab. And the response is a snapshot
/// of the batch as the POSTs returned: should a poller event that already settled
/// a job land first, replaying the snapshot would roll that row back — and the
/// poller never re-sends a job it considers finished, so the row would stay
/// "Running" until a manual refresh.
pub(crate) fn merge_jobs(jobs: &mut Vec<JobReport>, incoming: impl IntoIterator<Item = JobReport>) {
    for job in incoming {
        match jobs.iter_mut().find(|j| j.id == job.id) {
            Some(slot) if slot.state.is_terminal() && !job.state.is_terminal() => {}
            Some(slot) => *slot = job,
            None => jobs.push(job),
        }
    }
}

/// The "Dispatching N/M…" counter after one `action:progress` event. Only the
/// dispatch stages move it: `dispatching` sets it and `dispatched` clears it. The
/// poller's `polling` / `settled` ticks — which arrive every 15 s for as long as
/// any earlier batch is unsettled, rows or not — leave it alone; clearing on them
/// dropped the label back to a bare "Dispatching…" mid-batch on every tick.
pub(crate) fn next_dispatch_progress(
    current: Option<(usize, usize)>,
    stage: &str,
    dispatched: usize,
    total: usize,
) -> Option<(usize, usize)> {
    match stage {
        "dispatching" => Some((dispatched, total)),
        "dispatched" => None,
        _ => current,
    }
}

/// What a Jobs-table row is keyed on: the job id plus every field a later report
/// for that job can change (status, exit code, duration, correlators, whether a
/// retry can be rebuilt). A keyed list builds a row's cells once per key, so a
/// progress event re-renders only the rows it actually advanced — the rest keep
/// their DOM and any keyboard focus. The other columns are fixed at dispatch.
pub(crate) type JobRowKey = (
    u64,
    String,
    Option<i32>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    bool,
);

pub(crate) fn job_row_key(job: &JobReport) -> JobRowKey {
    (
        job.id,
        // The label is distinct per variant and carries its message, so it also
        // decides the status pill's class.
        job.state.label(),
        job.exit_code,
        job.duration_seconds,
        job.activity_id,
        job.series_uid.clone(),
        job.request.is_some(),
    )
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

/// Why this job has no Retry, or `None` when it may be retried.
///
/// Only a **definite** failure qualifies. `Unknown` means the dispatch may have
/// reached the device — a timeout or a 5xx after send — and replaying it is exactly
/// what `ReplaySafety::ActOnce` exists to refuse: a second reboot, or a script run
/// twice. A job still in flight, or one that succeeded, has nothing to retry.
pub(crate) fn retry_blocked_reason(job: &JobReport) -> Option<String> {
    match &job.state {
        JobState::Failed(_) if job.request.is_some() => None,
        JobState::Failed(_) => {
            Some("This job did not record what it was sent, so it cannot be rebuilt.".into())
        }
        JobState::Unknown(_) => Some(
            "The outcome is unknown — the action may already have run on the device, so it is \
             never replayed. Check the device in NinjaOne first."
                .into(),
        ),
        JobState::TimedOut => Some(
            "NinjaOne never reported how this ended — the action may still have run on the \
             device, so it is never replayed. Check the device in NinjaOne first."
                .into(),
        ),
        JobState::Skipped(_) => Some(
            "Nothing was sent to this device. Select it again to plan a fresh dispatch.".into(),
        ),
        _ => Some("Only a failed job can be retried.".into()),
    }
}

/// What the Retry cell says when there is no Retry button: `(short, full)` for a
/// settled job that will not be retried, `None` while a job is still in flight, once
/// it completed, or when it can be retried. An empty cell beside a red "Timed out"
/// read as a missing button.
pub(crate) fn retry_cell_note(job: &JobReport) -> Option<(&'static str, String)> {
    let short = match job.state {
        JobState::Failed(_) | JobState::Unknown(_) | JobState::TimedOut => "Not retried",
        JobState::Skipped(_) => "Not sent",
        JobState::Queued | JobState::Running | JobState::Completed => return None,
    };
    retry_blocked_reason(job).map(|full| (short, full))
}

/// Rebuilds the dispatch that produced `jobs`, for their devices only.
///
/// The result is a plain `ActionRequest` with no confirm token, so a retry goes
/// through the normal plan → confirm flow: a fresh payload-bound approval, and every
/// `plan()` guardrail re-run against current state. Nothing here dispatches.
///
/// Each device gets back only its **own** targets, exactly as the original batch
/// sent them — never the union across the jobs being retried. The maintenance-window
/// override is never carried (`override_window` stays false): it was approved for
/// the original moment, not this one.
///
/// Several jobs are rebuilt into one request only if they were dispatched with the
/// same kind, dry-run flag and options — i.e. rows of one batch. Anything else is
/// refused rather than merged, since one of them would silently run with the
/// other's options.
pub(crate) fn retry_request(jobs: &[&JobReport]) -> Result<ActionRequest, String> {
    let Some(first) = jobs.first() else {
        return Err("No failed jobs to retry.".into());
    };
    for job in jobs {
        if let Some(why) = retry_blocked_reason(job) {
            return Err(format!("{}: {why}", job.device_name));
        }
    }
    // `retry_blocked_reason` has vouched for every `request` being present.
    let shared = |j: &JobReport| {
        j.request.as_ref().map(|r| JobRequest {
            targets: Vec::new(),
            ..r.clone()
        })
    };
    let base = shared(first);
    if jobs
        .iter()
        .any(|j| j.kind != first.kind || j.dry_run != first.dry_run || shared(j) != base)
    {
        return Err(
            "These jobs were dispatched with different actions or options — retry them one at \
             a time."
                .into(),
        );
    }
    let base = base.unwrap_or_default();

    let mut req = ActionRequest::new(first.kind, jobs.iter().map(|j| j.device_id).collect());
    req.device_targets = jobs
        .iter()
        .filter_map(|j| {
            let targets = &j.request.as_ref()?.targets;
            (!targets.is_empty()).then(|| (j.device_id, targets.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    req.script_id = base.script_id;
    req.script_uid = base.script_uid;
    req.script_name = base.script_name;
    req.parameters = base.parameters;
    req.run_as = base.run_as;
    req.reboot = base.reboot;
    req.reboot_mode = base.reboot_mode;
    req.reason = base.reason;
    req.include_offline = base.include_offline;
    req.dry_run = first.dry_run;
    Ok(req)
}

/// A batch with more than one retryable job, for the Jobs tab's "Retry failed".
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RetryBatch {
    pub batch_id: u64,
    pub detail: String,
    pub job_ids: Vec<u64>,
}

/// Every batch holding two or more retryable jobs, newest batch first. A batch with
/// a single failure is served by that row's own Retry button.
pub(crate) fn retryable_batches(jobs: &[JobReport]) -> Vec<RetryBatch> {
    let mut by_batch: BTreeMap<u64, RetryBatch> = BTreeMap::new();
    for job in jobs.iter().filter(|j| retry_blocked_reason(j).is_none()) {
        by_batch
            .entry(job.batch_id)
            .or_insert_with(|| RetryBatch {
                batch_id: job.batch_id,
                detail: job.detail.clone(),
                job_ids: Vec::new(),
            })
            .job_ids
            .push(job.id);
    }
    by_batch
        .into_values()
        .rev()
        .filter(|b| b.job_ids.len() > 1)
        .collect()
}

/// What a job was told to install, for its Jobs row: `(short, full)`, the first
/// three targets then "and N more", with the whole list for the tooltip. `None`
/// when it was sent no list (a native endpoint, a reboot, a script sent hand-typed
/// parameters).
///
/// Selective patching is the point of the remediation kinds, and once dispatched
/// the per-device list existed only on the job's recorded request.
pub(crate) fn job_targets_summary(job: &JobReport) -> Option<(String, String)> {
    const SHOWN: usize = 3;
    let targets = &job.request.as_ref()?.targets;
    if targets.is_empty() {
        return None;
    }
    let full = targets.join(", ");
    let short = if targets.len() > SHOWN {
        format!(
            "{}, and {} more",
            targets[..SHOWN].join(", "),
            targets.len() - SHOWN
        )
    } else {
        full.clone()
    };
    Some((short, full))
}

/// What a remediation job's exit code means, `(short, long)`, from the exit-code
/// table of the toolkit's reference scripts (`remediation/README.md`, which pins
/// the same codes). `None` for other kinds, whose scripts define their own codes,
/// and for a code the reference scripts never return.
///
/// NinjaOne's v2 API returns no script output, so the code is all the Jobs tab
/// has; a bare `4` beside "Failed: FAILURE" read as a failed install.
pub(crate) fn remediation_exit_meaning(
    kind: ActionKind,
    code: i32,
) -> Option<(&'static str, &'static str)> {
    if !kind.is_remediation() {
        return None;
    }
    Some(match code {
        0 => (
            "done",
            "Every listed patch is installed or already was (on a dry run: would install or \
             already is).",
        ),
        1 => (
            "bad input",
            "The parameters were missing, malformed or empty. Nothing was changed.",
        ),
        2 => (
            "nothing matched",
            "None of the listed patches is offered to this device or installed on it.",
        ),
        3 => (
            "incomplete",
            "At least one listed patch failed, is hidden on the device, or wasn't offered.",
        ),
        4 => (
            "restart held back",
            "A restart is pending and rebootBehavior=Never held it back. For OS patches it may \
             have been pending before this run.",
        ),
        5 => (
            "environment error",
            "Windows Update was unavailable or busy, the session wasn't elevated, or the \
             install hook failed.",
        ),
        10 => (
            "no install mechanism",
            "The software script's install hook isn't implemented yet, so nothing was \
             installed. See remediation/README.md.",
        ),
        _ => return None,
    })
}
