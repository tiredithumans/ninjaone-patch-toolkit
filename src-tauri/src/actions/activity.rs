//! Resolving a dispatched job from the `/activities` feed: `match_activity`, `advance_job`.

use super::*;
use crate::model::{Activity, ActivityOutcome};
use chrono::{DateTime, Utc};
use std::collections::HashSet;

/// Whether an activity type is one that `kind` can produce.
///
/// The third correlation tier is the *only* path open to the native endpoints
/// (`scan`/`apply`/`reboot`), which return no correlator at all, so the type is most
/// of what that tier has to go on — and it is chosen per kind:
///
/// * an OS scan/apply emits `PATCH_MANAGEMENT`, a software one
///   `SOFTWARE_PATCH_MANAGEMENT`;
/// * a reboot emits `SYSTEM`;
/// * a script — hand-picked or a remediation — emits `SCRIPTING` for a library
///   script and `ACTION`/`ACTIONSET` for a built-in. `SCRIPT` is not in the spec's
///   `activityType` enum but is kept: an accepted-but-never-sent value costs
///   nothing, while a missing one costs a hung job.
///
/// This used to be one list for every kind, including the broad `SYSTEM`,
/// `CONDITION_ACTION`/`CONDITION_ACTIONSET` and `SCHEDULED_TASK`. The last three are
/// NinjaOne's own policy and scheduler runs, not anything this app dispatched, so a
/// condition firing on the device after a dispatch could resolve a script job with
/// its verdict — and an OS apply could be resolved by a software apply's activity,
/// or a script by an unrelated `SYSTEM` event.
fn is_action_activity(kind: ActionKind, activity_type: Option<&str>) -> bool {
    let Some(t) = activity_type else {
        return false;
    };
    match kind {
        ActionKind::OsPatchScan | ActionKind::OsPatchApply => t == "PATCH_MANAGEMENT",
        ActionKind::SoftwarePatchScan | ActionKind::SoftwarePatchApply => {
            t == "SOFTWARE_PATCH_MANAGEMENT"
        }
        ActionKind::Reboot => t == "SYSTEM",
        ActionKind::Script | ActionKind::OsPatchRemediate | ActionKind::SoftwarePatchRemediate => {
            matches!(t, "SCRIPTING" | "SCRIPT" | "ACTION" | "ACTIONSET")
        }
    }
}

/// Finds the activity that corresponds to a dispatched job.
///
/// Three tiers, most to least certain: the exact activity id the dispatch returned,
/// the activity series uid, then the newest activity on that device since dispatch
/// of a type the job's kind emits (see [`is_action_activity`]). The middle tier is what makes an id-less dispatch response usable —
/// several tenants return only a `jobUid`.
///
/// `claimed` carries the activity ids already bound to *other* jobs, and only the
/// third tier consults it. That tier is pure heuristic — device + activity type +
/// a timestamp floor — and the native endpoints (`scan`/`apply`/`reboot`) return no
/// correlator at all, so every one of those jobs reaches it. Without the exclusion,
/// two actions dispatched to the same device close together both select the newest
/// matching activity and swap each other's `exit_code`/`activity_id`. The first two
/// tiers are exact bindings and are deliberately *not* filtered: a job that already
/// owns an id must keep resolving to it on every later poll.
pub fn match_activity<'a>(
    activities: &'a [Activity],
    job: &JobReport,
    claimed: &HashSet<i64>,
) -> Option<&'a Activity> {
    if let Some(id) = job.activity_id
        && let Some(a) = activities.iter().find(|a| a.id == Some(id))
    {
        return Some(a);
    }
    if let Some(uid) = job.series_uid.as_deref()
        && let Some(a) = activities
            .iter()
            .find(|a| a.series_uid.as_deref() == Some(uid))
    {
        return Some(a);
    }
    // Allow a little slack around the dispatch timestamp for clock skew between
    // this machine and the NinjaOne backend.
    let floor = (job.dispatched_ts - 5) as f64;
    activities
        .iter()
        .filter(|a| is_action_activity(job.kind, a.activity_type.as_deref()))
        .filter(|a| a.activity_time.unwrap_or(0.0) >= floor)
        // An activity with no id cannot be tracked, so it cannot be excluded either;
        // it stays eligible exactly as before.
        .filter(|a| a.id.is_none_or(|id| !claimed.contains(&id)))
        .max_by(|a, b| {
            a.activity_time
                .unwrap_or(0.0)
                .total_cmp(&b.activity_time.unwrap_or(0.0))
        })
}

/// Advances one job given the activities visible for its device.
///
/// A poll that returned *no* activities is not a failure — the feed lags behind a
/// dispatch — so the job holds its state until [`JOB_TIMEOUT_MINUTES`] elapses.
///
/// `claimed` is threaded through a whole poll pass: it is seeded with the ids
/// already bound to other jobs and gains this job's id as soon as one is bound, so
/// no two jobs can resolve to the same activity. Correlation is recorded on the
/// *first* match rather than only on a terminal one — otherwise a long-running job
/// re-ran the third-tier heuristic on every tick and could land on a different
/// activity each time.
pub fn advance_job(
    job: &mut JobReport,
    activities: &[Activity],
    now: DateTime<Utc>,
    claimed: &mut HashSet<i64>,
) {
    match match_activity(activities, job, claimed) {
        Some(a) if a.is_terminal() => {
            let state = match a.outcome() {
                ActivityOutcome::Succeeded => JobState::Completed,
                ActivityOutcome::TimedOut => JobState::TimedOut,
                ActivityOutcome::Failed(code) => JobState::Failed(code),
            };
            if job.activity_id.is_none() {
                job.activity_id = a.id;
            }
            if job.series_uid.is_none() {
                job.series_uid = a.series_uid.clone();
            }
            job.exit_code = a.exit_code();
            job.finish(state, now);
        }
        Some(a) => {
            if job.activity_id.is_none() {
                job.activity_id = a.id;
            }
            if job.series_uid.is_none() {
                job.series_uid = a.series_uid.clone();
            }
            // A device that keeps producing matching non-terminal activities used to
            // hold this job at Running forever: the timeout lived only in the other
            // two arms. Such a job pinned the poller (`release_job_poller_if_idle`
            // keeps it alive while any job is non-terminal) and could never be
            // evicted by `append_jobs`'s MAX_JOBS trim, which retains non-terminals.
            if job.is_past_timeout(now) {
                job.finish(JobState::TimedOut, now);
            } else {
                job.state = JobState::Running;
            }
        }
        None => {
            if job.is_past_timeout(now) {
                job.finish(JobState::TimedOut, now);
            }
        }
    }
    if let Some(id) = job.activity_id {
        claimed.insert(id);
    }
}
