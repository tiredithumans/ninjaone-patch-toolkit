//! The dispatch itself: one POST per eligible device, bounded and audited.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use tauri::AppHandle;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::warn;

use super::{ActionProgressEvent, ActionRequest, emit_progress};
use crate::actions::{ActionKind, JobReport, JobState, PlannedTarget, audit, fmt_ts};
use crate::api::actions::{ScriptDispatch, ScriptRef};
use crate::api::{NinjaApiClient, is_outcome_unknown};
use crate::model::{PatchType, RebootMode};
use crate::state::AppState;

/// The part of a dispatch that is identical for every device in a batch.
///
/// Held behind one `Arc` and shared with each spawned task. The loop used to clone
/// all of this per device — six `String`/`Option` clones plus the `PlannedTarget`
/// — which scales with fleet size on every batch for data that cannot differ
/// within one.
pub(super) struct DispatchContext {
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
    pub(super) detail: String,
    pub(super) instance: String,
    pub(super) client_id: Option<String>,
    pub(super) confirm_prefix: Option<String>,
    pub(super) batch_id: u64,
    pub(super) id_base: u64,
}

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
                emit_progress(
                    app,
                    ActionProgressEvent {
                        batch_id: ctx.batch_id,
                        stage: "dispatching",
                        dispatched: done,
                        total: dispatched.len(),
                        jobs: vec![job.clone()],
                    },
                );
                dispatched[index] = Some(job);
            }
            Err(err) => warn!(?err, "a dispatch task panicked"),
        }
    }
    dispatched.into_iter().flatten().collect()
}

/// Audits, dispatches and records the outcome for a single device.
async fn dispatch_one(
    ctx: &DispatchContext,
    target: &PlannedTarget,
    index: usize,
    sem: &Semaphore,
) -> JobReport {
    let dispatched_at = Utc::now();
    let mut job = JobReport {
        id: ctx.id_base + index as u64,
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
    };

    // Written before the request goes out, so a crash mid-batch still leaves
    // evidence of what was attempted.
    audit::record_off_runtime(vec![audit::AuditEntry {
        timestamp: audit::now_stamp(),
        instance: ctx.instance.clone(),
        client_id: ctx.client_id.clone(),
        batch_id: ctx.batch_id,
        job_id: job.id,
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
            .filter(|p| !p.is_empty())
            .map(|p| audit::redact_parameters(p)),
        dry_run: ctx.dry_run,
        confirm_token_prefix: ctx.confirm_prefix.clone(),
        outcome: "dispatching".into(),
        activity_id: None,
        series_uid: None,
        exit_code: None,
    }])
    .await;

    let outcome = {
        let _permit = sem.acquire().await;
        send_action(ctx, target.device_id).await
    };
    record_dispatch(&mut job, outcome, Utc::now());

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
            ctx.api
                .run_script(device_id, sref, params, &ctx.run_as)
                .await
                .map(Some)
        }
    }
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
