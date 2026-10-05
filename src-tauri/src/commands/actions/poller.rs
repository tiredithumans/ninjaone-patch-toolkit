//! The background job poller that walks dispatched jobs to a terminal state.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tauri::{AppHandle, Manager};
use tokio::sync::Semaphore;
use tracing::warn;

use super::dispatch::invalidate_after;
use super::{ActionProgressEvent, emit_progress};
use crate::actions::{ActionKind, JobReport, JobState, audit};
use crate::api::NinjaApiClient;
use crate::model::Activity;
use crate::state::{AppState, JobSession};

/// How often the poller re-reads the activity feed for unresolved jobs.
const POLL_INTERVAL_SECS: u64 = 15;

/// How many `/activities` reads one tick keeps in flight. The default dispatch
/// concurrency, but not that setting: an operator who lowers "Dispatch
/// concurrency" to send cautiously would otherwise also slow every status check of
/// a large batch to one read at a time. Reads change nothing on a device.
const MAX_FEED_READS_IN_FLIGHT: usize = 8;

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
        // Series uids the feed has shown on a real activity (see `feed_reads`).
        // Lives with the poller task: a fresh poller starts empty and simply reads
        // device-wide until it has seen each one again.
        let mut confirmed_series: HashSet<String> = HashSet::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(POLL_INTERVAL_SECS)).await;

            let (session, pending, api) = {
                let state = app.state::<AppState>();
                let (session, pending) = state.pending_jobs();
                (session, pending, state.api.clone())
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

            poll_tick(&app, &session, pending, api, &mut confirmed_series).await;
        }
    });
}

/// One pass over the unresolved jobs: read each device's activity feed, advance
/// every job it resolves, invalidate what a settled action changed, close out the
/// audit records and tell the frontend.
async fn poll_tick(
    app: &AppHandle,
    session: &JobSession,
    pending: Vec<JobReport>,
    api: NinjaApiClient,
    confirmed_series: &mut HashSet<String>,
) {
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
    let updates = resolve_pending(
        &api,
        pending,
        &mut claimed,
        confirmed_series,
        now,
        MAX_FEED_READS_IN_FLIGHT,
    )
    .await;

    let tick = settle_tick(&app.state::<AppState>(), session, updates);
    // Close out the audit records opened at dispatch, now that the outcome and exit
    // code are known. Collected first and written in one pass: the poller settles a
    // whole batch at a time, so a per-job write reopened the log once per device.
    audit::record_off_runtime(tick.closing).await;
    emit_progress(
        app,
        ActionProgressEvent {
            batch_id: 0,
            stage: if tick.settled_any {
                "settled"
            } else {
                "polling"
            },
            dispatched: 0,
            total: 0,
            jobs: tick.applied,
        },
    );
}

/// What a tick hands back to the async half of [`poll_tick`]: the rows to emit and
/// the closing audit records to write.
pub(super) struct SettledTick {
    /// Updates the job store accepted — the only ones the frontend is told about.
    pub(super) applied: Vec<JobReport>,
    /// Whether any applied update reached a terminal state.
    pub(super) settled_any: bool,
    pub(super) closing: Vec<audit::AuditEntry>,
}

/// The synchronous half of a tick: apply the resolved updates under `session` —
/// the one [`AppState::pending_jobs`] read them under — and decide what to
/// invalidate, emit and audit.
///
/// A tick awaits a feed read per device, and a sign-out, sign-in or tenant switch
/// can land in that time. `apply_job_updates` already refused the departed
/// session's rows, but the tick went on regardless: it invalidated the *new*
/// session's caches for them, emitted them to the frontend (whose merge re-added
/// them to the new session's Jobs tab), and labelled their closing audit records
/// with the instance read from Settings *now*. The emit now covers only what was
/// applied.
///
/// Invalidation follows the *read*, not the row. After a same-instance sign-in the
/// device really did change and the next session's caches hold that same tenant's
/// data, so they are dropped for it; after a tenant switch the verdict came from
/// the other instance and the caches belong to it, so nothing is dropped.
///
/// The closing audit record is written only for an applied row, labelled with the
/// session it was dispatched in. A row that was not applied is one the session's
/// end removed (the Jobs tab's Clear keeps unsettled rows), and `clear_jobs`
/// already closed it as unresolved, so a verdict here would be a second close.
/// Either the tick applies first (the row is terminal and `clear_jobs` skips it) or
/// the clear does (the tick writes nothing): one close per job.
pub(super) fn settle_tick(
    state: &AppState,
    session: &JobSession,
    updates: Vec<JobReport>,
) -> SettledTick {
    let applied_ids = state.apply_job_updates(session, updates.clone());
    let read_on_this_tenant = state.is_session_tenant(session);
    let mut applied = Vec::with_capacity(applied_ids.len());
    let mut closing = Vec::new();
    // Deduped so a 200-device batch does not bump the epochs 200 times; a handful
    // of variants makes a Vec the right container.
    let mut invalidated: Vec<ActionKind> = Vec::new();
    let mut settled_any = false;
    for job in updates {
        let was_applied = applied_ids.contains(&job.id);
        if job.state.is_terminal() {
            settled_any |= was_applied;
            if was_applied || read_on_this_tenant {
                // Patch state changes on completion, not on dispatch — and only for
                // the kinds that actually changed something. Same rule as the
                // dispatch site, via the same function.
                if !invalidated.contains(&job.kind) {
                    invalidated.push(job.kind);
                    invalidate_after(job.kind, job.dry_run, state);
                }
            }
            if was_applied {
                closing.push(audit::AuditEntry::closing(
                    &job,
                    session.instance().to_string(),
                    session.client_id().map(str::to_string),
                ));
            }
        }
        if was_applied {
            applied.push(job);
        }
    }
    SettledTick {
        applied,
        settled_any,
        closing,
    }
}

/// One `/activities` read in a tick: the device it covers, the series it may be
/// narrowed to, and the time floor applied to what comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FeedRead {
    pub(super) device_id: i64,
    pub(super) series_uid: Option<String>,
    pub(super) since_ts: i64,
}

/// Plans a tick's reads: **one per device**, however many of its jobs are pending.
///
/// This used to be one read per *job*, so a device carrying a scan, an apply and a
/// reboot was asked for the same feed three times a tick — yet the feed is per
/// device, and every job on it is matched against the same list. The floor is the
/// earliest of those jobs' dispatch times, less the same 5 s skew allowance
/// `match_activity` uses, so no job loses an activity it could have matched; each
/// job's own floor still applies inside the third-tier heuristic.
///
/// A read is narrowed to `seriesUid` only when the device has exactly one pending
/// job and that job's series uid has already been *seen on an activity*
/// (`confirmed_series`). A dispatch response's uid is not proof on its own —
/// `parse_dispatch_response` takes a bare `uid` as a last resort, which may be an
/// echoed script uid no activity carries — and a read narrowed to a series that
/// never appears would starve the job until its timeout, where the device-wide read
/// would have resolved it through the third tier.
pub(super) fn feed_reads(
    pending: &[JobReport],
    confirmed_series: &HashSet<String>,
) -> Vec<FeedRead> {
    let mut by_device: BTreeMap<i64, Vec<&JobReport>> = BTreeMap::new();
    for job in pending {
        by_device.entry(job.device_id).or_default().push(job);
    }
    by_device
        .into_iter()
        .map(|(device_id, jobs)| {
            let series_uid = match jobs.as_slice() {
                [only] => only
                    .series_uid
                    .clone()
                    .filter(|uid| confirmed_series.contains(uid)),
                _ => None,
            };
            let earliest = jobs.iter().map(|j| j.dispatched_ts).min().unwrap_or(0);
            FeedRead {
                device_id,
                series_uid,
                since_ts: earliest - 5,
            }
        })
        .collect()
}

/// Reads each device's feed once and advances every pending job against it.
///
/// The reads fan out; the correlation that consumes them does not. `advance_job`
/// runs strictly in `pending` order because `claimed` is threaded through it: the
/// third-tier heuristic binds an activity to whichever job reaches it first, and no
/// two jobs may claim the same one. Fanning out the *resolution* as well would make
/// which job wins depend on network timing. Two jobs on one device now read the
/// very same list, which is the case that exclusion was written for.
///
/// `confirmed_series` gains every pending job's series uid that its device's feed
/// shows on a real activity, so later ticks may narrow to it (see [`feed_reads`]).
///
/// At most `max_reads` reads are in flight at once, as `dispatch_batch` bounds the
/// POSTs. They were all spawned at once, so a 500-device batch put 500 GETs on the
/// wire every tick, and when NinjaOne answered with 429s every read parked on its
/// `Retry-After` and retried together, so each tick ran long and hit the limit
/// again.
pub(super) async fn resolve_pending(
    api: &NinjaApiClient,
    pending: Vec<JobReport>,
    claimed: &mut HashSet<i64>,
    confirmed_series: &mut HashSet<String>,
    now: DateTime<Utc>,
    max_reads: usize,
) -> Vec<JobReport> {
    let mut feeds: HashMap<i64, anyhow::Result<Vec<Activity>>> = HashMap::new();
    let sem = Arc::new(Semaphore::new(max_reads.max(1)));
    let mut set = tokio::task::JoinSet::new();
    for read in feed_reads(&pending, confirmed_series) {
        let api = api.clone();
        let sem = Arc::clone(&sem);
        set.spawn(async move {
            let _permit = sem.acquire().await;
            let result = api
                .activities(
                    Some(read.device_id),
                    read.series_uid.as_deref(),
                    Some(read.since_ts),
                )
                .await;
            (read.device_id, result)
        });
    }
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((device_id, result)) => {
                feeds.insert(device_id, result);
            }
            Err(err) => warn!(?err, "activity poll task failed"),
        }
    }

    for job in &pending {
        if let (Some(uid), Some(Ok(list))) = (job.series_uid.as_deref(), feeds.get(&job.device_id))
            && list.iter().any(|a| a.series_uid.as_deref() == Some(uid))
        {
            confirmed_series.insert(uid.to_string());
        }
    }

    let mut updates = Vec::with_capacity(pending.len());
    for mut job in pending {
        match feeds.get(&job.device_id) {
            Some(Ok(list)) => crate::actions::advance_job(&mut job, list, now, claimed),
            Some(Err(err)) => {
                // A transient failure to *read* the feed is not a failure of the
                // job. Hold state and let the timeout decide.
                warn!(?err, device_id = job.device_id, "activity poll failed");
                if job.is_past_timeout(now) {
                    job.finish(JobState::TimedOut, now);
                }
            }
            // The join failed (panic or cancellation). Same treatment: this says
            // nothing about the job, so hold state and let the timeout decide
            // rather than inventing an outcome.
            None => {
                if job.is_past_timeout(now) {
                    job.finish(JobState::TimedOut, now);
                }
            }
        }
        updates.push(job);
    }
    updates
}
