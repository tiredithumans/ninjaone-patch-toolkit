//! The dispatch itself: one POST per eligible device, bounded and audited.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use tauri::AppHandle;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::warn;

use super::{ActionProgressEvent, ActionRequest, emit_progress};
use crate::actions::{ActionKind, JobReport, JobRequest, JobState, PlannedTarget, audit, fmt_ts};
use crate::api::actions::{ScriptDispatch, ScriptRef};
use crate::api::{NinjaApiClient, SendGuard, is_outcome_unknown, is_send_refused};
use crate::model::{PatchType, RebootMode};
use crate::state::AppState;

/// The part of a dispatch that is identical for every device in a batch.
///
/// Held behind one `Arc` and shared with each spawned task. The loop used to clone
/// all of this per device — six `String`/`Option` clones plus the `PlannedTarget`
/// — which scales with fleet size on every batch for data that cannot differ
/// within one.
pub(super) struct DispatchContext {
    /// Carries `still_current` as its send guard, so a retry after a 429 or 401
    /// is stopped too — see [`NinjaApiClient::with_send_guard`].
    pub(super) api: NinjaApiClient,
    pub(super) kind: ActionKind,
    pub(super) script: Option<ScriptRef>,
    pub(super) run_as: String,
    /// Device id → its `parameters` string. The one genuinely per-device field in
    /// here: a remediation script is told which patches to install *on that device*.
    pub(super) parameters: BTreeMap<i64, String>,
    pub(super) reason: String,
    pub(super) reboot_mode: RebootMode,
    pub(super) dry_run: bool,
    /// The plan's `window_overridden`: this batch goes out only because the operator
    /// overrode a closed maintenance window. Audited on every opening record.
    pub(super) window_overridden: bool,
    pub(super) detail: String,
    pub(super) instance: String,
    pub(super) client_id: Option<String>,
    pub(super) confirm_prefix: Option<String>,
    pub(super) batch_id: u64,
    pub(super) id_base: u64,
    /// Device id → what its job records for a retry. Per device only because the
    /// targets are.
    pub(super) job_requests: BTreeMap<i64, JobRequest>,
    /// Whether the session `run_action` started in is still the live one
    /// (`AppState::job_session_is_current` on the session it sampled). Asked before
    /// every send, because a batch wider than the semaphore queues devices for as
    /// long as the ones ahead of them take — and a sign-out or tenant switch in that
    /// time used to leave the rest of the departed session's batch POSTing on.
    pub(super) still_current: SendGuard,
}

/// What a device that was still queued when the session ended records instead of
/// a send.
pub(super) const NOT_SENT_SESSION_ENDED: &str =
    "not sent: you signed in again or switched instance while the batch was dispatching";

/// Dispatches every eligible target concurrently (bounded by `permits`), emitting
/// progress as each completes, and returns the jobs in the plan's order.
///
/// Extracted from `run_action`, which had grown to 278 lines fusing seven
/// responsibilities. This is the safety-critical stretch — it is what actually
/// reaches a device — so it is worth reading on its own rather than as the middle
/// third of a command handler.
pub(super) async fn dispatch_batch(
    app: &AppHandle,
    ctx: Arc<DispatchContext>,
    eligible: &[PlannedTarget],
    permits: usize,
) -> Vec<JobReport> {
    emit_progress(
        app,
        ActionProgressEvent {
            batch_id: ctx.batch_id,
            stage: "dispatching",
            dispatched: 0,
            total: eligible.len(),
            jobs: Vec::new(),
        },
    );

    // Opening records for the whole batch in one write, before anything is sent, so
    // a crash mid-batch still leaves evidence of what was attempted. This was one
    // record per device, written by its own task before it waited for a permit: a
    // 500-device batch opened the log 500 times at once on the blocking pool.
    let started = Utc::now();
    audit::record_off_runtime(
        eligible
            .iter()
            .enumerate()
            .map(|(index, target)| opening_record(&ctx, target, ctx.id_base + index as u64))
            .collect(),
    )
    .await;

    let sem = Arc::new(Semaphore::new(permits));
    let mut set: JoinSet<(usize, JobReport)> = JoinSet::new();
    for (index, target) in eligible.iter().enumerate() {
        let ctx = ctx.clone();
        let sem = sem.clone();
        let target = target.clone();
        set.spawn(async move { (index, dispatch_one(&ctx, &target, index, &sem).await) });
    }

    let mut dispatched: Vec<Option<JobReport>> = vec![None; eligible.len()];
    let mut done = 0usize;
    while let Some(res) = set.join_next().await {
        match res {
            Ok((index, job)) => {
                done += 1;
                if let Some(ev) = device_progress(&ctx, done, dispatched.len(), &job) {
                    emit_progress(app, ev);
                }
                dispatched[index] = Some(job);
            }
            Err(err) => warn!(?err, "a dispatch task panicked"),
        }
    }
    fill_unreported(&ctx, eligible, dispatched, started)
}

/// The batch's jobs in plan order, with every slot a panicked task left empty
/// filled in.
///
/// Such a device's POST may already have gone out, so it gets a job all the same —
/// `Unknown`, polled like any other ambiguous send — rather than vanishing with no
/// row, no poller entry, and an opening audit record that nothing ever closes.
/// `slots` is indexed like `eligible`, so the filled job carries the id its task
/// would have used.
pub(super) fn fill_unreported(
    ctx: &DispatchContext,
    eligible: &[PlannedTarget],
    slots: Vec<Option<JobReport>>,
    started: chrono::DateTime<Utc>,
) -> Vec<JobReport> {
    slots
        .into_iter()
        .zip(eligible)
        .enumerate()
        .map(|(index, (slot, target))| {
            slot.unwrap_or_else(|| unrecorded_job(ctx, target, ctx.id_base + index as u64, started))
        })
        .collect()
}

/// What a device whose dispatch task failed before reporting records as its state.
pub(super) const DISPATCH_TASK_PANICKED: &str =
    "the dispatch task failed before recording an outcome; the request may have been sent";

/// The job for a device whose dispatch task panicked before it reported.
///
/// Dated from the batch start rather than from now: the poller's activity floor is
/// `dispatched_ts` less a skew, and the send — if it happened — came after the
/// batch began, so this floor cannot exclude it.
pub(super) fn unrecorded_job(
    ctx: &DispatchContext,
    target: &PlannedTarget,
    job_id: u64,
    started: chrono::DateTime<Utc>,
) -> JobReport {
    let mut job = queued_job(ctx, target, job_id, started);
    job.state = JobState::Unknown(DISPATCH_TASK_PANICKED.into());
    job
}

/// The audit record written before a device's request goes out.
pub(super) fn opening_record(
    ctx: &DispatchContext,
    target: &PlannedTarget,
    job_id: u64,
) -> audit::AuditEntry {
    audit::AuditEntry {
        timestamp: audit::now_stamp(),
        instance: ctx.instance.clone(),
        client_id: ctx.client_id.clone(),
        batch_id: ctx.batch_id,
        job_id,
        kind: ctx.kind,
        device_id: target.device_id,
        device_name: target.device_name.clone(),
        organization: target.organization.clone(),
        detail: ctx.detail.clone(),
        // This device's own parameters, so the audit trail records what each device
        // was actually told to install rather than a batch-wide approximation.
        parameters: ctx
            .parameters
            .get(&target.device_id)
            .and_then(|p| audit::Redacted::of(p)),
        dry_run: ctx.dry_run,
        window_override: ctx.window_overridden,
        confirm_token_prefix: ctx.confirm_prefix.clone(),
        outcome: "dispatching".into(),
        activity_id: None,
        series_uid: None,
        exit_code: None,
    }
}

/// The progress event for one device's outcome — `None` once the session that
/// dispatched it has ended.
///
/// The frontend merges every row an event carries into its Jobs list. After a
/// sign-out it has already cleared that list for the next session, so a send
/// still finishing (or a queued device recorded as not sent) re-added the departed
/// session's rows there, Failed ones with a Retry button.
pub(super) fn device_progress(
    ctx: &DispatchContext,
    done: usize,
    total: usize,
    job: &JobReport,
) -> Option<ActionProgressEvent> {
    (ctx.still_current)().then(|| ActionProgressEvent {
        batch_id: ctx.batch_id,
        stage: "dispatching",
        dispatched: done,
        total,
        jobs: vec![job.clone()],
    })
}

/// Dispatches to a single device and records the outcome. Its opening audit record
/// was written by [`dispatch_batch`], with the rest of the batch's, before any task
/// was spawned.
async fn dispatch_one(
    ctx: &DispatchContext,
    target: &PlannedTarget,
    index: usize,
    sem: &Semaphore,
) -> JobReport {
    let job_id = ctx.id_base + index as u64;
    let job = send_and_record(ctx, target, job_id, sem).await;

    // A job settled at dispatch (NinjaOne rejected the request outright) never
    // reaches the poller, which writes every other closing record — so without this
    // the trail could not tell "rejected at send time" from "sent, and never
    // reported back".
    if job.state.is_terminal() {
        audit::record_off_runtime(vec![audit::AuditEntry::closing(
            &job,
            ctx.instance.clone(),
            ctx.client_id.clone(),
        )])
        .await;
    }
    job
}

/// Waits for a permit, sends, and builds the device's job from that turn — the
/// part of [`dispatch_one`] between its two audit writes.
///
/// Separate so a test can drive it without writing to the operator's audit log or
/// needing an `AppHandle`: the job's dispatch time must come from the turn, and
/// nothing else pins that.
pub(super) async fn send_and_record(
    ctx: &DispatchContext,
    target: &PlannedTarget,
    job_id: u64,
    sem: &Semaphore,
) -> JobReport {
    let turn = send_if_current(ctx, target.device_id, sem).await;
    let mut job = queued_job(ctx, target, job_id, turn.at);

    match turn.outcome {
        Some(outcome) => record_dispatch(&mut job, outcome, Utc::now()),
        // Terminal, so `dispatch_one`'s closing record says it never went out.
        None => job.finish(JobState::Skipped(NOT_SENT_SESSION_ENDED.into()), Utc::now()),
    }
    job
}

/// A device's job as dispatch first records it: queued, dated `dispatched_at`.
fn queued_job(
    ctx: &DispatchContext,
    target: &PlannedTarget,
    job_id: u64,
    dispatched_at: chrono::DateTime<Utc>,
) -> JobReport {
    JobReport {
        id: job_id,
        batch_id: ctx.batch_id,
        device_id: target.device_id,
        device_name: target.device_name.clone(),
        organization: target.organization.clone(),
        kind: ctx.kind,
        detail: ctx.detail.clone(),
        dry_run: ctx.dry_run,
        state: JobState::Queued,
        dispatched_at: fmt_ts(dispatched_at),
        dispatched_ts: dispatched_at.timestamp(),
        finished_at: None,
        duration_seconds: None,
        activity_id: None,
        series_uid: None,
        exit_code: None,
        request: ctx.job_requests.get(&target.device_id).cloned(),
    }
}

/// One device's turn at the semaphore: when it came, and what the send said.
pub(super) struct SendTurn {
    /// Taken once the permit is held and the session re-checked, i.e. as the POST
    /// goes out (for a device not sent, as its turn came). It is the job's
    /// `dispatched_at`.
    pub(super) at: chrono::DateTime<Utc>,
    /// `None` when the session ended while the device waited: nothing was sent.
    pub(super) outcome: Option<anyhow::Result<Option<ScriptDispatch>>>,
}

/// Sends to one device once a permit is free — unless the session ended while it
/// waited, in which case nothing is sent and the outcome is `None`.
///
/// The check sits after the permit on purpose: that wait is the long one. So does
/// the dispatch time. It used to be taken before the wait, and with 8 permits, a
/// 45 s request timeout and up to 500 devices, the last devices of a batch could
/// queue for many minutes. That time came off their 45-minute job timeout, and the
/// poller's activity floor (`dispatched_ts` less 5 s) reached back far enough for
/// the third-tier heuristic to bind an activity older than the send.
pub(super) async fn send_if_current(
    ctx: &DispatchContext,
    device_id: i64,
    sem: &Semaphore,
) -> SendTurn {
    let _permit = sem.acquire().await;
    let current = (ctx.still_current)();
    let at = Utc::now();
    let outcome = if current {
        Some(send_action(ctx, device_id).await)
    } else {
        None
    };
    SendTurn { at, outcome }
}

/// Records what the dispatch POST said on the job.
///
/// Classified on the error's *type*: [`is_outcome_unknown`] marks every failure
/// after which the action may still have reached NinjaOne (a transport failure after
/// send, a 5xx, an unreadable 2xx body). Those become `Unknown` — polled, never
/// auto-retried. This used to match `"may already"` in the message, a phrase only
/// the timeout arm produced, so a gateway 5xx on an apply read as a definite failure
/// while the device could be installing patches. Anything else — a 4xx, a connect
/// failure, a refusal in `send_action` — is a definite `Failed`.
pub(super) fn record_dispatch(
    job: &mut JobReport,
    outcome: anyhow::Result<Option<ScriptDispatch>>,
    now: chrono::DateTime<Utc>,
) {
    match outcome {
        Ok(dispatch) => {
            if let Some(d) = dispatch {
                job.activity_id = d.any_id();
                job.series_uid = d.series_uid.clone();
            }
            job.state = JobState::Running;
        }
        Err(err) if is_outcome_unknown(&err) => job.state = JobState::Unknown(err.to_string()),
        // The guard stopped a retry after a definite rejection: nothing reached the
        // device, so this is "not sent" rather than a failure worth retrying.
        Err(err) if is_send_refused(&err) => {
            job.finish(JobState::Skipped(NOT_SENT_SESSION_ENDED.into()), now);
        }
        Err(err) => job.finish(JobState::Failed(err.to_string()), now),
    }
}

/// The POST itself, per [`ActionKind`].
///
/// The dry-run refusal stays here, at the dispatch site, rather than only in
/// `plan()`. `plan()` already blocks a dry run on a kind with no preview mode and
/// `run_action` refuses a blocked plan — but that put "a dry run never mutates a
/// device" two files away from the POSTs that would do the mutating, resting
/// entirely on `supports_dry_run()` being right. A new `ActionKind` that answers
/// it wrongly would otherwise dispatch for real while the UI said "Dry run".
async fn send_action(
    ctx: &DispatchContext,
    device_id: i64,
) -> anyhow::Result<Option<ScriptDispatch>> {
    if ctx.dry_run && !ctx.kind.supports_dry_run() {
        return Err(anyhow::anyhow!(
            "refusing to dispatch \"{}\" as a dry run: it has no preview mode",
            ctx.kind.label()
        ));
    }
    match ctx.kind {
        ActionKind::OsPatchScan => ctx
            .api
            .device_patch_scan(device_id, PatchType::Os)
            .await
            .map(|_| None),
        ActionKind::SoftwarePatchScan => ctx
            .api
            .device_patch_scan(device_id, PatchType::Software)
            .await
            .map(|_| None),
        ActionKind::OsPatchApply => ctx
            .api
            .device_patch_apply(device_id, PatchType::Os)
            .await
            .map(|_| None),
        ActionKind::SoftwarePatchApply => ctx
            .api
            .device_patch_apply(device_id, PatchType::Software)
            .await
            .map(|_| None),
        ActionKind::Reboot => ctx
            .api
            .device_reboot(device_id, ctx.reboot_mode, &ctx.reason)
            .await
            .map(|_| None),
        // The remediation kinds are dispatched exactly like a hand-driven script —
        // they differ only in where the script ref and the parameters came from.
        ActionKind::Script | ActionKind::OsPatchRemediate | ActionKind::SoftwarePatchRemediate => {
            let Some(sref) = ctx.script.as_ref() else {
                return Err(anyhow::anyhow!("no script selected"));
            };
            // An empty allow list would install nothing while reporting success, so
            // it fails here too rather than only in `plan()` — same defense-in-depth
            // as the dry-run refusal above.
            let params = ctx.parameters.get(&device_id).map_or("", String::as_str);
            if ctx.kind.is_remediation() && params.is_empty() {
                return Err(anyhow::anyhow!(
                    "refusing to dispatch \"{}\" with no target list for this device",
                    ctx.kind.label()
                ));
            }
            // A dry run is only a dry run if the script is *told* so. `plan()` allows
            // one only for composed parameters (which always carry the flag) and a
            // script that declares it; this is the same fact checked where it matters.
            if ctx.dry_run && !carries_dry_run_flag(params) {
                return Err(anyhow::anyhow!(
                    "refusing to dispatch \"{}\" as a dry run: its parameters do not set dryRun=true",
                    ctx.kind.label()
                ));
            }
            ctx.api
                .run_script(device_id, sref, params, &ctx.run_as)
                .await
                .map(Some)
        }
    }
}

/// Whether a parameter string tells the script to preview — the exact token
/// `actions::build_parameters` composes, as NinjaOne splits it (on spaces).
pub(super) fn carries_dry_run_flag(params: &str) -> bool {
    params.split(' ').any(|t| t == "dryRun=true")
}

/// Drops the caches a completed action has invalidated.
///
/// One decision, called from both the dispatch site and the job poller. They used to
/// hold separate copies of this rule and the copies disagreed: dispatch invalidated
/// the device inventory only for `Reboot`, while the poller invalidated it for every
/// settled batch — including scans, which change nothing. So an apply left
/// `os.needsReboot` stale for up to the 15-minute device TTL if the poller happened
/// not to run, and a scan triggered a spurious whole-fleet device refetch.
pub(super) fn invalidate_after(kind: ActionKind, dry_run: bool, state: &AppState) {
    // A dry run previews a mutating kind without touching the device, so the
    // caches are as fresh after it as before. Dropping them here cost a
    // whole-fleet refetch per preview — and `dry_run` is the default.
    if dry_run || !kind.is_mutating() {
        return;
    }
    // The device's pending list is about to change, and the 120 s current-patch TTL
    // would otherwise keep serving pre-action data.
    state.invalidate_current_patches();
    if kind.can_reboot() {
        // A reboot — or an install that sets the pending-reboot flag — moves
        // os.needsReboot, which lives in the 15-minute device cache.
        state.invalidate_fleet_devices();
    }
}

pub(super) fn action_detail(req: &ActionRequest) -> String {
    match req.kind {
        ActionKind::Script => req
            .script_name
            .clone()
            .or_else(|| req.script_id.map(|id| format!("Script #{id}")))
            .unwrap_or_else(|| "Script".into()),
        ActionKind::Reboot => format!(
            "Reboot ({})",
            req.reboot_mode.unwrap_or(RebootMode::Normal).api_value()
        ),
        // A remediation runs a library script, so name it the way the `Script` arm
        // does. This fell through to the bare label, which meant the Jobs tab and
        // the audit log recorded "Apply selected OS patches" for every remediation
        // without ever saying *which* script did it — and the script is configured
        // in Settings, so the operator cannot infer it from the request either.
        kind if kind.is_remediation() => match req.script_name.as_deref() {
            Some(name) => format!("{} — {name}", kind.label()),
            None => kind.label().to_string(),
        },
        other => other.label().to_string(),
    }
}
