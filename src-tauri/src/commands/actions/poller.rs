//! The background job poller that walks dispatched jobs to a terminal state.

use std::collections::HashSet;

use chrono::Utc;
use tauri::{AppHandle, Manager};
use tracing::warn;

use super::dispatch::invalidate_after;
use super::{ActionProgressEvent, emit_progress};
use crate::actions::{ActionKind, JobReport, JobState, audit};
use crate::api::NinjaApiClient;
use crate::state::AppState;

/// How often the poller re-reads the activity feed for unresolved jobs.
const POLL_INTERVAL_SECS: u64 = 15;

/// Background poller that walks unresolved jobs to a terminal state.
///
/// Only one runs at a time; a batch dispatched while it is working simply joins
/// the pending set. The `AppState` lock is re-acquired at each synchronous touch
/// point and never held across an `.await`.
pub(super) fn spawn_job_poller(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        // Held for the life of the task: dropping it — on the `return` below, on a
        // panic anywhere in the loop, or on runtime shutdown — releases the slot.
        let Some(mut claim) = app.state::<AppState>().try_claim_job_poller() else {
            return;
        };
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS)).await;

            let (pending, api) = {
                let state = app.state::<AppState>();
                (state.pending_jobs(), state.api.clone())
            };
            if pending.is_empty() {
                // Release and exit only if still idle when checked under the jobs
                // lock; otherwise a batch dispatched since the snapshot above would
                // be left with no poller running (see `release_job_poller_if_idle`).
                match app.state::<AppState>().release_job_poller_if_idle(claim) {
                    None => return,
                    Some(held) => {
                        claim = held;
                        continue;
                    }
                }
            }

            poll_tick(&app, pending, api).await;
        }
    });
}

/// One pass over the unresolved jobs: read each device's activity feed, advance
/// every job it resolves, invalidate what a settled action changed, close out the
/// audit records and tell the frontend.
async fn poll_tick(app: &AppHandle, pending: Vec<JobReport>, api: NinjaApiClient) {
    let now = Utc::now();
    // Activity ids already bound to a job, so the third-tier correlation
    // heuristic can't hand the same activity to two jobs. Seeded from the
    // whole store rather than just `pending`: a job that already settled
    // still owns its activity, and the native endpoints return no
    // correlator at all, so every scan/apply/reboot reaches that tier.
    let mut claimed: HashSet<i64> = app
        .state::<AppState>()
        .jobs_snapshot()
        .iter()
        .filter_map(|j| j.activity_id)
        .collect();
    // The feed reads fan out; the correlation that consumes them does not.
    //
    // This was a serial `await` per job, so a tick cost the sum of every
    // pending job's round trip and grew linearly with the batch — while
    // dispatch on the very same path already uses a `JoinSet`. The reads are
    // independent, so they run together.
    //
    // `advance_job` still runs strictly in `pending` order below, because
    // `claimed` is threaded through it: the third-tier heuristic binds an
    // activity to whichever job reaches it first, and no two jobs may claim
    // the same one. Fanning out the *resolution* as well would make which job
    // wins depend on network timing.
    let feeds = {
        let mut set = tokio::task::JoinSet::new();
        for (idx, job) in pending.iter().enumerate() {
            let api = api.clone();
            let device_id = job.device_id;
            let since = job.dispatched_ts - 5;
            set.spawn(async move { (idx, api.activities(Some(device_id), Some(since)).await) });
        }
        let mut out: Vec<Option<_>> = (0..pending.len()).map(|_| None).collect();
        while let Some(joined) = set.join_next().await {
            match joined {
                Ok((idx, result)) => out[idx] = Some(result),
                Err(err) => warn!(?err, "activity poll task failed"),
            }
        }
        out
    };

    let mut updates = Vec::new();
    for (mut job, feed) in pending.into_iter().zip(feeds) {
        match feed {
            Some(Ok(list)) => crate::actions::advance_job(&mut job, &list, now, &mut claimed),
            Some(Err(err)) => {
                // A transient failure to *read* the feed is not a failure
                // of the job. Hold state and let the timeout decide.
                warn!(?err, device_id = job.device_id, "activity poll failed");
                if job.is_past_timeout(now) {
                    job.finish(JobState::TimedOut, now);
                }
            }
            // The join failed (panic or cancellation). Same treatment: this
            // says nothing about the job, so hold state and let the timeout
            // decide rather than inventing an outcome.
            None => {
                if job.is_past_timeout(now) {
                    job.finish(JobState::TimedOut, now);
                }
            }
        }
        updates.push(job);
    }

    let settled: Vec<JobReport> = updates
        .iter()
        .filter(|j| j.state.is_terminal())
        .cloned()
        .collect();
    let settings = {
        let state = app.state::<AppState>();
        state.apply_job_updates(updates.clone());
        // Patch state changes on completion, not on dispatch — and only for
        // the kinds that actually changed something. Same rule as the
        // dispatch site, via the same function.
        // Deduped so a 200-device batch does not bump the epochs 200 times;
        // a handful of variants makes a Vec the right container.
        let mut seen: Vec<ActionKind> = Vec::new();
        for job in settled.iter() {
            if !seen.contains(&job.kind) {
                seen.push(job.kind);
                invalidate_after(job.kind, job.dry_run, &state);
            }
        }
        state.settings_snapshot()
    };
    // Close out the audit record opened at dispatch, now that the outcome
    // and exit code are known.
    // Collected first and written in one pass: the poller settles a whole
    // batch at a time, so a per-job write reopened the log once per device.
    let closing = settled
        .iter()
        .map(|job| {
            audit::AuditEntry::closing(
                job,
                settings.instance_base_url.clone(),
                settings.client_id.clone(),
            )
        })
        .collect();
    audit::record_off_runtime(closing).await;
    emit_progress(
        app,
        ActionProgressEvent {
            batch_id: 0,
            stage: if settled.is_empty() {
                "polling"
            } else {
                "settled"
            },
            dispatched: 0,
            total: 0,
            jobs: updates,
        },
    );
}
