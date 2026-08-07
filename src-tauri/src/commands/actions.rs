//! IPC surface for device actions.
//!
//! The flow is deliberately two-step. `plan_action` reports exactly what would
//! happen — which devices, which are skipped and why, what will restart, and the
//! literal `parameters` string — and issues a single-use confirmation token bound
//! to that request. `run_action` re-plans from scratch and refuses to dispatch
//! unless the token still matches, so a tampered device list or a dialog left open
//! too long fails closed.
//!
//! Every guardrail is enforced here rather than in the webview: a stale or
//! modified frontend must not be able to talk the backend into a wider blast
//! radius than Settings allows.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{Local, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, State};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::actions::{
    ActionKind, ActionPlan, JobReport, JobState, PlanInput, PlannedTarget, RebootChoice, audit,
    fmt_ts, plan,
};
use crate::api::NinjaApiClient;
use crate::api::actions::{ScriptDispatch, ScriptRef};
use crate::error::UiError;
use crate::model::{AutomationScript, Device, PatchType, RebootMode};
use crate::state::AppState;

/// How often the poller re-reads the activity feed for unresolved jobs.
const POLL_INTERVAL_SECS: u64 = 15;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionRequest {
    pub kind: ActionKind,
    pub device_ids: Vec<i64>,
    /// KBs (OS) or product titles (software) for a script that accepts an allow
    /// list, keyed by the device that gets them. Ignored by the native endpoints,
    /// which have no per-patch variant.
    ///
    /// A device is sent only the patches ticked *on it*. This replaced a batch-wide
    /// `targets: Vec<String>` that handed every device the union of the selection, so
    /// a device received KBs it did not have and the operator's "install this patch
    /// on that device" became "install every selected patch everywhere" — invisible
    /// in the confirmation dialog, since the one parameter string it showed looked
    /// correct for whichever device you checked it against. Don't reintroduce a
    /// batch-wide list; a genuinely uniform string is what `parameters` is for.
    #[serde(default)]
    pub device_targets: HashMap<i64, Vec<String>>,
    #[serde(default)]
    pub script_id: Option<i64>,
    #[serde(default)]
    pub script_uid: Option<String>,
    #[serde(default)]
    pub script_name: Option<String>,
    /// Forwarded to NinjaOne verbatim and shown character-for-character in the
    /// confirmation dialog. When absent for a script, it is composed from
    /// `targets` + `reboot`.
    #[serde(default)]
    pub parameters: Option<String>,
    #[serde(default)]
    pub run_as: Option<String>,
    #[serde(default)]
    pub reboot: RebootChoice,
    #[serde(default)]
    pub reboot_mode: Option<RebootMode>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub include_offline: bool,
    #[serde(default)]
    pub override_window: bool,
    #[serde(default)]
    pub dry_run: bool,
    /// Echoed back from `plan_action`.
    #[serde(default)]
    pub confirm_token: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionBatch {
    pub batch_id: u64,
    pub dispatched: usize,
    pub skipped: usize,
    pub jobs: Vec<JobReport>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptSummary {
    pub id: i64,
    pub name: String,
    pub description: Option<String>,
    pub language: Option<String>,
    pub operating_systems: Vec<String>,
    /// See [`AutomationScript::accepts_kb_allow_list`] — gates whether the UI may
    /// offer per-KB targeting for this script.
    pub accepts_kb_allow_list: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAsOptions {
    pub roles: Vec<String>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActionProgressEvent {
    batch_id: u64,
    /// `dispatching` | `dispatched` | `polling` | `settled`
    stage: &'static str,
    dispatched: usize,
    total: usize,
    /// Rows whose state changed since the last event; empty on a pure stage tick.
    jobs: Vec<JobReport>,
}

fn emit_progress(app: &AppHandle, ev: ActionProgressEvent) {
    let _ = app.emit("action:progress", ev);
}

/// Guardrails 1 and 2: the feature must be switched on, and the current grant must
/// actually carry `management`. Checked per command so a stale frontend cannot
/// bypass either.
fn require_actions_enabled(state: &AppState) -> Result<(), UiError> {
    if !state.settings_snapshot().actions.enabled {
        return Err(UiError::new(
            "Patch actions are disabled. Enable them in Settings → Patch actions.",
        ));
    }
    match state.auth.management_grant() {
        Some(true) => Ok(()),
        Some(false) => Err(UiError::new(
            "Your NinjaOne sign-in is read-only. Choose Re-authorize to grant the Management \
             scope, which patch actions require.",
        )),
        None => Err(UiError::new(
            "Could not confirm that your NinjaOne sign-in grants the Management scope. \
             Re-authorize to be sure patch actions will be accepted.",
        )),
    }
}

/// Stable fingerprint of everything that determines what would be dispatched **and
/// everything the guardrails react to**.
///
/// A confirmation token is only honored alongside a matching hash, so editing the
/// device list (or the parameters, the reboot mode, the run-as identity, or either
/// guardrail toggle) after the dialog opened invalidates the approval instead of
/// silently widening it.
///
/// The request is destructured exhaustively so that adding a field to
/// `ActionRequest` fails to compile here rather than silently falling outside the
/// binding — which is exactly how `include_offline`, `override_window` and `run_as`
/// came to be missing. The first two are the flags `plan()` uses to gate the
/// offline-queue warning and the maintenance-window blocker, so a token issued
/// under one blast radius validated under a wider one; `run_as` selects the
/// execution identity sent to NinjaOne, so an approval for `system` validated after
/// being switched to a stored credential.
fn request_hash(req: &ActionRequest, parameters: &str, script: Option<&ScriptRef>) -> String {
    let ActionRequest {
        kind,
        device_ids,
        // Covered by the `parameters` argument, which is the canonical per-device
        // rendering these compose into — the only form that reaches NinjaOne.
        device_targets: _,
        script_id,
        script_uid,
        // Display-only and audit-only fields are deliberately excluded: they change
        // nothing about what is dispatched or what the guardrails say.
        script_name: _,
        // The *effective* parameters are hashed via the `parameters` argument, which
        // is what actually goes on the wire.
        parameters: _,
        run_as,
        reboot,
        reboot_mode,
        reason: _,
        include_offline,
        override_window,
        dry_run,
        // The token being validated cannot be part of its own fingerprint.
        confirm_token: _,
    } = req;

    let mut ids = device_ids.clone();
    ids.sort_unstable();
    ids.dedup();

    let mut hasher = Sha256::new();
    // Every field is followed by a separator byte that cannot occur in the encoded
    // values, so no two different requests can concatenate to the same input (e.g.
    // parameters "a" ‖ "b" vs "ab" ‖ "").
    let mut field = |bytes: &[u8]| {
        hasher.update(bytes);
        hasher.update([0x1f]);
    };

    field(format!("{kind:?}").as_bytes());
    field(
        ids.iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",")
            .as_bytes(),
    );
    field(&script_id.unwrap_or_default().to_le_bytes());
    field(script_uid.as_deref().unwrap_or_default().as_bytes());
    // The *resolved* script — for a remediation kind it comes from Settings rather
    // than the request, so without this an id edited between the dialog opening and
    // the confirm would run a different script under the same approval.
    field(
        match script {
            Some(ScriptRef::Script { id }) => format!("script:{id}"),
            Some(ScriptRef::Action { uid }) => format!("action:{uid}"),
            None => String::new(),
        }
        .as_bytes(),
    );
    field(parameters.as_bytes());
    field(run_as.as_deref().unwrap_or_default().as_bytes());
    field(format!("{reboot:?}").as_bytes());
    field(format!("{reboot_mode:?}").as_bytes());
    field(&[u8::from(*include_offline)]);
    field(&[u8::from(*override_window)]);
    field(&[u8::from(*dry_run)]);
    hex(&hasher.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn random_token() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The `parameters` string sent to each device, keyed by device id.
///
/// `BTreeMap` rather than `HashMap` so the canonical rendering below — and thus the
/// confirmation hash — does not depend on iteration order.
///
/// Three shapes, one function, because they must agree: `Script` sends one
/// hand-composed string to every device; a remediation kind composes a *distinct*
/// string per device from that device's ticked patches; a native endpoint takes no
/// parameters at all.
fn per_device_parameters(req: &ActionRequest) -> BTreeMap<i64, String> {
    if !req.kind.runs_a_script() {
        return BTreeMap::new();
    }
    // A hand-written string is batch-wide by nature and is sent verbatim — the
    // toolkit never rewrites what the operator typed. The remediation kinds have no
    // field to type it in, and honoring one there would silently discard the
    // per-device targeting that is their entire purpose.
    if !req.kind.is_remediation()
        && let Some(verbatim) = req
            .parameters
            .as_ref()
            .map(|p| p.trim())
            .filter(|p| !p.is_empty())
    {
        return req
            .device_ids
            .iter()
            .map(|id| (*id, verbatim.to_string()))
            .collect();
    }
    // Otherwise compose from that device's own targets. `device_targets` is empty
    // when nothing is being targeted (KB targeting off, or a script that takes no
    // allow list), which yields the same empty-list string for every device.
    req.device_ids
        .iter()
        .map(|id| {
            let targets = req.device_targets.get(id).cloned().unwrap_or_default();
            (
                *id,
                crate::actions::build_parameters(req.kind, &targets, req.reboot, req.dry_run),
            )
        })
        .collect()
}

/// The per-device parameters as one canonical string, for the confirmation hash.
///
/// Every string that will reach NinjaOne appears here exactly once, bound to the
/// device it will be sent to, so re-ticking a single row on a single device
/// invalidates the approval.
///
/// Each value is **length-prefixed**. A separator alone is not enough here: unlike
/// the fields `request_hash` joins, a parameter string can be typed by hand in the
/// script picker, so `{1: "a\u{1e}2=b"}` would otherwise render identically to
/// `{1: "a", 2: "b"}` — two different dispatches sharing one approval.
fn canonical_parameters(params: &BTreeMap<i64, String>) -> String {
    params
        .iter()
        .map(|(id, p)| format!("{id}:{}:{p}", p.len()))
        .collect::<Vec<_>>()
        .join("\u{1e}")
}

/// What the operator is shown in the confirmation dialog.
///
/// One line per device when the strings differ (remediation), the bare string when
/// they don't (a hand-driven script) — the toolkit never sends a `parameters` string
/// the operator has not seen, and with per-device targeting that means all of them.
fn parameters_preview(
    params: &BTreeMap<i64, String>,
    eligible: &[PlannedTarget],
) -> Option<String> {
    if params.is_empty() {
        return None;
    }
    let mut distinct = params.values().collect::<BTreeSet<_>>();
    if distinct.len() <= 1 {
        return distinct.pop_first().cloned();
    }
    Some(
        eligible
            .iter()
            .filter_map(|t| {
                params
                    .get(&t.device_id)
                    .map(|p| format!("{} → {p}", t.device_name))
            })
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

/// Eligible devices that would be dispatched to with no target list of their own.
fn untargeted_names<'a>(
    eligible: &'a [PlannedTarget],
    device_targets: &HashMap<i64, Vec<String>>,
) -> Vec<&'a str> {
    eligible
        .iter()
        .filter(|t| !device_targets.contains_key(&t.device_id))
        .map(|t| t.device_name.as_str())
        .collect()
}

/// A device-name list for a warning, capped so a 25-device batch stays one sentence.
fn summarize_names(names: &[&str]) -> String {
    const SHOWN: usize = 5;
    if names.len() <= SHOWN {
        return names.join(", ");
    }
    format!(
        "{}, and {} more",
        names[..SHOWN].join(", "),
        names.len() - SHOWN
    )
}

/// The script a request will actually run. For a remediation kind it comes from
/// Settings, never from the request — a stale frontend must not be able to name its
/// own script and inherit the remediation kind's guardrails.
fn resolve_script(
    req: &ActionRequest,
    settings: &crate::settings::ActionSettings,
) -> Option<ScriptRef> {
    if req.kind.is_remediation() {
        return crate::actions::remediation_script_id(req.kind, settings)
            .map(|id| ScriptRef::Script { id });
    }
    match (req.script_id, req.script_uid.clone()) {
        (Some(id), _) => Some(ScriptRef::Script { id }),
        (None, Some(uid)) => Some(ScriptRef::Action { uid }),
        _ => None,
    }
}

/// A planned request, resolved down to exactly what would be dispatched.
///
/// The parameters and the script ride along with the plan because all three must be
/// derived from one pass: the confirmation hash covers them, and `run_action` sends
/// them. Recomputing any of them separately is how the approved request and the
/// dispatched one drift apart.
struct PlannedAction {
    plan: ActionPlan,
    /// Device id → the `parameters` string that device will be sent.
    parameters: BTreeMap<i64, String>,
    script: Option<ScriptRef>,
}

/// Shared planning path for `plan_action` and `run_action`, so the two can never
/// disagree about what the guardrails say.
async fn build_plan(state: &AppState, req: &ActionRequest) -> Result<PlannedAction, UiError> {
    let settings = state.settings_snapshot();
    let devices: Vec<Device> = state
        .fleet_devices(None)
        .await
        .map_err(UiError::from)?
        .as_ref()
        .clone();
    let (orgs, _, _) = state.lookups().await.map_err(UiError::from)?;
    let org_names: HashMap<i64, String> = orgs.iter().map(|o| (o.id, o.name.clone())).collect();

    // Only the targets belonging to devices actually in this request count — a
    // frontend that left stale entries in the map must not satisfy the "something is
    // selected" guardrail with patches for devices it is not dispatching to.
    let target_count = req
        .device_ids
        .iter()
        .filter_map(|id| req.device_targets.get(id))
        .map(|t| t.len())
        .sum();

    let mut p = plan(PlanInput {
        kind: req.kind,
        device_ids: &req.device_ids,
        devices: &devices,
        org_names: &org_names,
        settings: &settings.actions,
        include_offline: req.include_offline,
        override_window: req.override_window,
        reboot_mode: req.reboot_mode,
        reboot: req.reboot,
        dry_run: req.dry_run,
        target_count,
        now: Local::now(),
    });

    let parameters = per_device_parameters(req);
    p.parameters_preview = parameters_preview(&parameters, &p.eligible);
    let script = resolve_script(req, &settings.actions);

    // Request-shape problems the pure planner can't see, since they depend on
    // settings and on which script was picked.
    if req.kind == ActionKind::Script && req.script_id.is_none() && req.script_uid.is_none() {
        p.blockers
            .push("No script selected. Choose one from the automation library.".into());
    }
    // A hand-picked script with KB targeting on, dispatched to a device that has
    // nothing ticked, receives an empty allow list. Not a blocker — the operator
    // chose those devices and the script may do something useful without a list —
    // but a remediation script would install nothing on them, and the per-device
    // preview alone is easy to skim past when it runs to 25 lines.
    if req.kind == ActionKind::Script && !req.device_targets.is_empty() {
        let empty = untargeted_names(&p.eligible, &req.device_targets);
        if !empty.is_empty() {
            p.warnings.push(format!(
                "{} device(s) have no selected patches and would be sent an empty allow list ({}). \
                 A script that only installs from that list will do nothing on them.",
                empty.len(),
                summarize_names(&empty)
            ));
        }
    }
    if req.kind == ActionKind::Reboot
        && req
            .reason
            .as_ref()
            .map(|r| r.trim().is_empty())
            .unwrap_or(true)
    {
        // The reason lands in NinjaOne's own activity feed, so it doubles as a
        // server-side audit record the toolkit can't forge or lose.
        p.blockers.push(
            "A reboot needs a stated reason — it is recorded in NinjaOne's activity feed.".into(),
        );
    }
    Ok(PlannedAction {
        plan: p,
        parameters,
        script,
    })
}

/// Reports what an action would do, without doing it.
#[tauri::command]
pub async fn plan_action(
    state: State<'_, AppState>,
    request: ActionRequest,
) -> Result<ActionPlan, UiError> {
    require_actions_enabled(&state)?;
    let PlannedAction {
        mut plan,
        parameters,
        script,
    } = build_plan(&state, &request).await?;

    // A blocked plan has nothing to confirm, so it gets no token. Scans are not
    // mutating and skip confirmation entirely.
    if !plan.is_blocked() && request.kind.is_mutating() {
        let token = random_token();
        state.store_pending_confirm(
            token.clone(),
            request_hash(
                &request,
                &canonical_parameters(&parameters),
                script.as_ref(),
            ),
        );
        plan.confirm_token = Some(token);
    }
    Ok(plan)
}

/// Dispatches an action to every eligible device.
#[tauri::command]
pub async fn run_action(
    state: State<'_, AppState>,
    app: AppHandle,
    request: ActionRequest,
) -> Result<ActionBatch, UiError> {
    require_actions_enabled(&state)?;

    // Re-plan rather than trusting anything the frontend computed.
    let PlannedAction {
        plan: p,
        parameters,
        script,
    } = build_plan(&state, &request).await?;
    if p.is_blocked() {
        return Err(UiError::new(p.blockers.join(" ")));
    }

    if request.kind.is_mutating() {
        let token = request.confirm_token.as_deref().unwrap_or_default();
        if !state.consume_confirm_token(
            token,
            &request_hash(
                &request,
                &canonical_parameters(&parameters),
                script.as_ref(),
            ),
        ) {
            return Err(UiError::new(
                "This action was not confirmed, or the confirmation expired or no longer matches \
                 the selection. Review the plan and confirm again.",
            ));
        }
    }

    let settings = state.settings_snapshot();
    let run_as = request
        .run_as
        .clone()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or_else(|| settings.actions.run_as.clone());
    let detail = action_detail(&request);

    let (batch_id, id_base) = state.next_job_ids(p.eligible.len() + p.skipped.len());
    let now = Utc::now();
    let mut jobs: Vec<JobReport> = Vec::with_capacity(p.eligible.len() + p.skipped.len());

    // Skipped targets are recorded as rows too, so the Jobs tab shows the *whole*
    // truth about a batch rather than quietly dropping devices.
    for (i, s) in p.skipped.iter().enumerate() {
        jobs.push(JobReport {
            id: id_base + p.eligible.len() as u64 + i as u64,
            batch_id,
            device_id: s.device_id,
            device_name: s.device_name.clone(),
            organization: String::new(),
            kind: request.kind,
            detail: detail.clone(),
            dry_run: request.dry_run,
            state: JobState::Skipped(s.reason.clone()),
            dispatched_at: fmt_ts(now),
            dispatched_ts: now.timestamp(),
            finished_at: Some(fmt_ts(now)),
            duration_seconds: Some(0),
            activity_id: None,
            series_uid: None,
            exit_code: None,
        });
    }

    emit_progress(
        &app,
        ActionProgressEvent {
            batch_id,
            stage: "dispatching",
            dispatched: 0,
            total: p.eligible.len(),
            jobs: Vec::new(),
        },
    );

    // Everything that does not vary per device is built once and shared, so a
    // 25-device batch stops re-cloning the script ref, run-as identity, parameter
    // string, detail line, instance URL and client id 25 times over.
    let ctx = Arc::new(DispatchContext {
        api: state.api.clone(),
        kind: request.kind,
        script,
        run_as,
        parameters,
        reason: request.reason.clone().unwrap_or_default(),
        reboot_mode: request.reboot_mode.unwrap_or(RebootMode::Normal),
        dry_run: request.dry_run,
        detail: detail.clone(),
        instance: settings.instance_base_url.clone(),
        client_id: settings.client_id.clone(),
        confirm_prefix: request
            .confirm_token
            .as_deref()
            .map(|t| t.chars().take(8).collect::<String>()),
        batch_id,
        id_base,
    });

    let dispatched = dispatch_batch(
        &app,
        ctx,
        &p.eligible,
        settings.actions.concurrency.clamp(1, 16),
    )
    .await;
    jobs.extend(dispatched);

    let live = jobs
        .iter()
        .filter(|j| !matches!(j.state, JobState::Skipped(_)))
        .count();
    state.append_jobs(jobs.clone());

    // The pending list is about to change on every device we touched, and the
    // 120 s current-patch TTL would otherwise keep serving pre-action data.
    if request.kind.is_mutating() && live > 0 {
        state.invalidate_current_patches();
        if request.kind == ActionKind::Reboot {
            // A reboot flips os.needsReboot, which lives in the 15-minute device
            // cache — long enough to render the reboot invisible.
            state.invalidate_fleet_devices();
        }
    }

    info!(
        batch_id,
        kind = ?request.kind,
        dispatched = live,
        skipped = p.skipped.len(),
        dry_run = request.dry_run,
        "action batch dispatched"
    );
    emit_progress(
        &app,
        ActionProgressEvent {
            batch_id,
            stage: "dispatched",
            dispatched: live,
            total: live,
            jobs: Vec::new(),
        },
    );

    spawn_job_poller(&app);

    Ok(ActionBatch {
        batch_id,
        dispatched: live,
        skipped: p.skipped.len(),
        jobs,
    })
}

/// The part of a dispatch that is identical for every device in a batch.
///
/// Held behind one `Arc` and shared with each spawned task. The loop used to clone
/// all of this per device — six `String`/`Option` clones plus the `PlannedTarget`
/// — which scales with fleet size on every batch for data that cannot differ
/// within one.
struct DispatchContext {
    api: NinjaApiClient,
    kind: ActionKind,
    script: Option<ScriptRef>,
    run_as: String,
    /// Device id → its `parameters` string. The one genuinely per-device field in
    /// here: a remediation script is told which patches to install *on that device*.
    parameters: BTreeMap<i64, String>,
    reason: String,
    reboot_mode: RebootMode,
    dry_run: bool,
    detail: String,
    instance: String,
    client_id: Option<String>,
    confirm_prefix: Option<String>,
    batch_id: u64,
    id_base: u64,
}

/// Dispatches every eligible target concurrently (bounded by `permits`), emitting
/// progress as each completes, and returns the jobs in the plan's order.
///
/// Extracted from `run_action`, which had grown to 278 lines fusing seven
/// responsibilities. This is the safety-critical stretch — it is what actually
/// reaches a device — so it is worth reading on its own rather than as the middle
/// third of a command handler.
async fn dispatch_batch(
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
    audit::record(&audit::AuditEntry {
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
    });

    let _permit = sem.acquire().await;
    let outcome = send_action(ctx, target.device_id).await;

    match outcome {
        Ok(dispatch) => {
            if let Some(d) = dispatch {
                job.activity_id = d.any_id();
                job.series_uid = d.series_uid.clone();
            }
            job.state = JobState::Running;
        }
        Err(err) => {
            let msg = err.to_string();
            // A timed-out POST may already be queued on the device, so it is
            // recorded as Unknown — polled, but never auto-retried.
            job.state = if msg.contains("may already") {
                JobState::Unknown(msg)
            } else {
                let failed = JobState::Failed(msg);
                job.finish(failed.clone(), Utc::now());
                failed
            };
        }
    }
    job
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

fn action_detail(req: &ActionRequest) -> String {
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
        other => other.label().to_string(),
    }
}

/// Background poller that walks unresolved jobs to a terminal state.
///
/// Only one runs at a time; a batch dispatched while it is working simply joins
/// the pending set. The `AppState` lock is re-acquired at each synchronous touch
/// point and never held across an `.await`.
fn spawn_job_poller(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        use tauri::Manager;
        if !app.state::<AppState>().try_claim_job_poller() {
            return;
        }
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
                if app.state::<AppState>().release_job_poller_if_idle() {
                    return;
                }
                continue;
            }

            let now = Utc::now();
            let mut updates = Vec::new();
            for mut job in pending {
                let since = job.dispatched_ts - 5;
                match api.activities(Some(job.device_id), Some(since)).await {
                    Ok(list) => crate::actions::advance_job(&mut job, &list, now),
                    Err(err) => {
                        // A transient failure to *read* the feed is not a failure
                        // of the job. Hold state and let the timeout decide.
                        warn!(?err, device_id = job.device_id, "activity poll failed");
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
                if !settled.is_empty() {
                    // Patch state changes on completion, not on dispatch.
                    state.invalidate_current_patches();
                    state.invalidate_fleet_devices();
                }
                state.settings_snapshot()
            };
            // Close out the audit record opened at dispatch, now that the outcome
            // and exit code are known.
            for job in &settled {
                audit::record(&audit::AuditEntry {
                    timestamp: audit::now_stamp(),
                    instance: settings.instance_base_url.clone(),
                    client_id: settings.client_id.clone(),
                    batch_id: job.batch_id,
                    job_id: job.id,
                    kind: job.kind,
                    device_id: job.device_id,
                    device_name: job.device_name.clone(),
                    organization: job.organization.clone(),
                    detail: job.detail.clone(),
                    parameters: None,
                    dry_run: job.dry_run,
                    confirm_token_prefix: None,
                    outcome: audit::AuditEntry::outcome_of(&job.state),
                    activity_id: job.activity_id,
                    series_uid: job.series_uid.clone(),
                    exit_code: job.exit_code,
                });
            }
            emit_progress(
                &app,
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
    });
}

#[tauri::command]
pub fn list_jobs(state: State<'_, AppState>) -> Vec<JobReport> {
    state.jobs_snapshot()
}

#[tauri::command]
pub fn clear_jobs(state: State<'_, AppState>) -> Vec<JobReport> {
    state.clear_jobs();
    Vec::new()
}

/// The tenant's automation-script library, projected for the picker.
#[tauri::command]
pub async fn list_scripts(state: State<'_, AppState>) -> Result<Vec<ScriptSummary>, UiError> {
    require_actions_enabled(&state)?;
    let scripts = state
        .api
        .automation_scripts()
        .await
        .map_err(UiError::from)?;
    // NinjaOne keeps deactivated entries in the library but won't run them, so
    // offering one would produce a dispatch that silently does nothing. An entry
    // with no `active` field at all is treated as usable.
    Ok(scripts
        .iter()
        .filter(|s| s.active.unwrap_or(true))
        .map(summarize)
        .collect())
}

fn summarize(s: &AutomationScript) -> ScriptSummary {
    ScriptSummary {
        id: s.id,
        name: s
            .name
            .clone()
            .unwrap_or_else(|| format!("Script #{}", s.id)),
        description: s.description.clone(),
        language: s.language.clone(),
        operating_systems: s.operating_systems.clone(),
        accepts_kb_allow_list: s.accepts_kb_allow_list(),
    }
}

/// Credential roles this device will accept for `runAs`.
#[tauri::command]
pub async fn list_run_as_options(
    state: State<'_, AppState>,
    device_id: i64,
) -> Result<RunAsOptions, UiError> {
    require_actions_enabled(&state)?;
    let opts = state
        .api
        .device_scripting_options(device_id)
        .await
        .map_err(UiError::from)?;
    Ok(RunAsOptions {
        roles: opts.credentials.roles,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `request_hash` with no resolved script, which is every case except the two
    /// remediation kinds.
    fn hash(req: &ActionRequest, parameters: &str) -> String {
        request_hash(req, parameters, None)
    }

    fn request(kind: ActionKind, ids: Vec<i64>) -> ActionRequest {
        ActionRequest {
            kind,
            device_ids: ids,
            device_targets: HashMap::new(),
            script_id: None,
            script_uid: None,
            script_name: None,
            parameters: None,
            run_as: None,
            reboot: RebootChoice::Never,
            reboot_mode: None,
            reason: None,
            include_offline: false,
            override_window: false,
            dry_run: false,
            confirm_token: None,
        }
    }

    /// Every field the guardrails read, or that reaches NinjaOne, must be bound to
    /// the token — otherwise an approval obtained under one blast radius validates
    /// under a wider one.
    #[test]
    fn request_hash_binds_every_guardrail_and_dispatch_input() {
        let base = request(ActionKind::Reboot, vec![1, 2]);
        let h = hash(&base, "");

        // `plan()` gates the offline-queue warning on this.
        let mut offline = request(ActionKind::Reboot, vec![1, 2]);
        offline.include_offline = true;
        assert_ne!(h, hash(&offline, ""), "include_offline must bind");

        // ...and the maintenance-window blocker on this.
        let mut window = request(ActionKind::Reboot, vec![1, 2]);
        window.override_window = true;
        assert_ne!(h, hash(&window, ""), "override_window must bind");

        // `run_as` is sent to NinjaOne verbatim as the execution identity, so an
        // approval for `system` must not validate against a stored credential.
        let mut elevated = request(ActionKind::Reboot, vec![1, 2]);
        elevated.run_as = Some("domain-admin".into());
        assert_ne!(h, hash(&elevated, ""), "run_as must bind");

        let mut reboot = request(ActionKind::Reboot, vec![1, 2]);
        reboot.reboot = RebootChoice::Auto;
        assert_ne!(h, hash(&reboot, ""), "reboot choice must bind");

        let mut mode = request(ActionKind::Reboot, vec![1, 2]);
        mode.reboot_mode = Some(RebootMode::Forced);
        assert_ne!(h, hash(&mode, ""), "reboot mode must bind");

        let mut dry = request(ActionKind::Reboot, vec![1, 2]);
        dry.dry_run = true;
        assert_ne!(h, hash(&dry, ""), "dry_run must bind");

        // The effective parameters are what actually go on the wire. `device_targets`
        // binds through them — see `the_confirmation_binds_every_devices_own_parameters`.
        assert_ne!(h, hash(&base, "dryRun=false"), "parameters must bind");
    }

    /// Field values are separated, so two different requests cannot concatenate
    /// into the same hash input.
    #[test]
    fn request_hash_is_not_confusable_across_field_boundaries() {
        // `parameters` and `run_as` are hashed adjacently, so a value that ends where
        // the next begins must not produce the same input as the pair shifted along.
        let mut c = request(ActionKind::Script, vec![1]);
        c.run_as = Some("y".into());
        let mut d = request(ActionKind::Script, vec![1]);
        d.run_as = None;
        assert_ne!(hash(&c, "x"), hash(&d, "xy"));
    }

    /// The per-device rendering is the hash's only view of `device_targets`, so no
    /// two distinct target maps may render to the same string.
    #[test]
    fn canonical_parameters_cannot_be_forged_across_devices() {
        // Device 12 with "x" vs device 1 with "2=x" — the id/value boundary.
        assert_ne!(
            canonical_parameters(&BTreeMap::from([(12, "x".to_string())])),
            canonical_parameters(&BTreeMap::from([(1, "2=x".to_string())]))
        );
        // ...and the boundary between two devices' entries, including a parameter
        // string that reproduces the rendering byte for byte. The operator can type
        // one of these by hand, so a separator alone would not be enough.
        let two_devices = canonical_parameters(&BTreeMap::from([
            (1, "a".to_string()),
            (2, "b".to_string()),
        ]));
        for forged in ["a\u{1e}2=b", "a\u{1e}2:1:b"] {
            assert_ne!(
                two_devices,
                canonical_parameters(&BTreeMap::from([(1, forged.to_string())])),
                "{forged} must not render as two devices' parameters"
            );
        }
    }

    #[test]
    fn request_hash_ignores_device_order_but_not_membership() {
        let a = request(ActionKind::Reboot, vec![3, 1, 2]);
        let b = request(ActionKind::Reboot, vec![1, 2, 3]);
        assert_eq!(hash(&a, ""), hash(&b, ""));

        // Adding a device must invalidate an approval issued for the smaller set.
        let c = request(ActionKind::Reboot, vec![1, 2, 3, 4]);
        assert_ne!(hash(&a, ""), hash(&c, ""));
    }

    #[test]
    fn request_hash_covers_the_fields_that_change_what_happens() {
        let base = request(ActionKind::Script, vec![1]);
        let baseline = hash(&base, "kbAllowList=1 dryRun=true");

        assert_ne!(
            baseline,
            hash(&base, "kbAllowList=999 dryRun=false"),
            "different parameters must not reuse an approval"
        );
        assert_ne!(
            baseline,
            hash(
                &ActionRequest {
                    dry_run: true,
                    ..base.clone()
                },
                "kbAllowList=1 dryRun=true"
            ),
            "flipping dry run must not reuse an approval"
        );
        assert_ne!(
            baseline,
            hash(
                &ActionRequest {
                    script_id: Some(7),
                    ..base.clone()
                },
                "kbAllowList=1 dryRun=true"
            ),
            "a different script must not reuse an approval"
        );
        assert_ne!(
            baseline,
            hash(
                &ActionRequest {
                    kind: ActionKind::Reboot,
                    ..base
                },
                "kbAllowList=1 dryRun=true"
            ),
            "a different action must not reuse an approval"
        );
    }

    #[test]
    fn effective_parameters_composes_only_for_scripts() {
        // A hand-picked script targets per device too, so its KB targeting cannot
        // drift back to handing every device the union of the selection.
        let mut req = request(ActionKind::Script, vec![1, 2]);
        req.device_targets =
            HashMap::from([(1, vec!["KB5040434".into()]), (2, vec!["KB5041580".into()])]);
        assert_eq!(
            per_device_parameters(&req),
            BTreeMap::from([
                (
                    1,
                    "kbAllowList=5040434 rebootBehavior=Never dryRun=false".to_string()
                ),
                (
                    2,
                    "kbAllowList=5041580 rebootBehavior=Never dryRun=false".to_string()
                ),
            ])
        );

        // A hand-written string is used verbatim — never silently rewritten — and it
        // is batch-wide by nature, which is the one place that is still correct.
        req.parameters = Some("  -Verbose  ".into());
        assert_eq!(
            per_device_parameters(&req),
            BTreeMap::from([(1, "-Verbose".into()), (2, "-Verbose".into())])
        );

        // KB targeting off: no per-device targets, so every device gets the same
        // empty allow list rather than one device's leaking onto another.
        let bare = request(ActionKind::Script, vec![1, 2]);
        let empty = "kbAllowList= rebootBehavior=Never dryRun=false";
        assert_eq!(
            per_device_parameters(&bare),
            BTreeMap::from([(1, empty.into()), (2, empty.into())])
        );

        // Native endpoints take no parameters at all.
        assert!(per_device_parameters(&request(ActionKind::Reboot, vec![1])).is_empty());
        assert!(per_device_parameters(&request(ActionKind::OsPatchApply, vec![1])).is_empty());
    }

    /// The devices a hand-picked script would reach with an empty allow list. They
    /// stay in the batch (the operator chose them), so the dialog has to name them.
    #[test]
    fn untargeted_devices_are_named_and_capped() {
        let eligible: Vec<PlannedTarget> = (1..=8)
            .map(|id| PlannedTarget {
                device_id: id,
                device_name: format!("srv-{id}"),
                organization: "Contoso".into(),
                offline: false,
            })
            .collect();
        let targets = HashMap::from([(1, vec!["KB1".to_string()])]);

        let names = untargeted_names(&eligible, &targets);
        assert_eq!(names.len(), 7, "every device but srv-1 lacks a target list");
        assert!(!names.contains(&"srv-1"));

        // Capped, so a 25-device batch stays one readable sentence.
        assert_eq!(
            summarize_names(&names),
            "srv-2, srv-3, srv-4, srv-5, srv-6, and 2 more"
        );
        assert_eq!(summarize_names(&["a", "b"]), "a, b");
    }

    /// The point of the remediation kinds: a device is told to install the patches
    /// ticked *on it*, not the union of the batch.
    #[test]
    fn remediation_parameters_are_scoped_to_each_device() {
        let mut req = request(ActionKind::OsPatchRemediate, vec![1, 2]);
        req.device_targets = HashMap::from([
            (1, vec!["KB5040434".into(), "KB5041580".into()]),
            (2, vec!["KB5041580".into()]),
        ]);

        let params = per_device_parameters(&req);
        assert_eq!(
            params[&1],
            "kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=false"
        );
        assert_eq!(
            params[&2],
            "kbAllowList=5041580 rebootBehavior=Never dryRun=false"
        );

        // A batch-wide override would discard exactly that scoping, so it is ignored
        // on this path rather than quietly widening every device's target list.
        req.parameters = Some("kbAllowList=999".into());
        assert_eq!(per_device_parameters(&req), params);

        // A device with nothing ticked gets an empty list, which `plan()` blocks and
        // `send_action` refuses — it must never silently inherit another device's.
        let mut partial = request(ActionKind::OsPatchRemediate, vec![1, 2]);
        partial.device_targets = HashMap::from([(1, vec!["KB5040434".into()])]);
        assert_eq!(
            per_device_parameters(&partial)[&2],
            "kbAllowList= rebootBehavior=Never dryRun=false"
        );
    }

    /// Per-device parameters must each be bound to the approval, or re-ticking one
    /// row on one device would reuse a token issued for a different install.
    #[test]
    fn the_confirmation_binds_every_devices_own_parameters() {
        let mut a = request(ActionKind::OsPatchRemediate, vec![1, 2]);
        a.device_targets =
            HashMap::from([(1, vec!["KB5040434".into()]), (2, vec!["KB5041580".into()])]);
        // The same two KBs, swapped between the two devices.
        let mut b = request(ActionKind::OsPatchRemediate, vec![1, 2]);
        b.device_targets =
            HashMap::from([(1, vec!["KB5041580".into()]), (2, vec!["KB5040434".into()])]);

        let canon = |r: &ActionRequest| canonical_parameters(&per_device_parameters(r));
        assert_ne!(
            hash(&a, &canon(&a)),
            hash(&b, &canon(&b)),
            "which device gets which patch must bind"
        );

        // The resolved script is not in the request at all for these kinds, so it is
        // hashed separately — an id edited in Settings mid-dialog must invalidate.
        assert_ne!(
            request_hash(&a, &canon(&a), Some(&ScriptRef::Script { id: 42 })),
            request_hash(&a, &canon(&a), Some(&ScriptRef::Script { id: 43 })),
            "the resolved remediation script must bind"
        );
    }

    /// The operator is shown every string that will be sent, which with per-device
    /// targeting means one line per device — but only when they actually differ.
    #[test]
    fn the_preview_shows_each_devices_own_parameters() {
        let eligible = vec![
            PlannedTarget {
                device_id: 1,
                device_name: "srv-a".into(),
                organization: "Contoso".into(),
                offline: false,
            },
            PlannedTarget {
                device_id: 2,
                device_name: "srv-b".into(),
                organization: "Contoso".into(),
                offline: false,
            },
        ];

        let differing = BTreeMap::from([
            (1, "kbAllowList=1".to_string()),
            (2, "kbAllowList=2".into()),
        ]);
        assert_eq!(
            parameters_preview(&differing, &eligible).as_deref(),
            Some("srv-a → kbAllowList=1\nsrv-b → kbAllowList=2")
        );

        // Identical strings collapse to one line — a hand-driven script would
        // otherwise repeat itself once per device for no information.
        let same = BTreeMap::from([(1, "-Verbose".to_string()), (2, "-Verbose".into())]);
        assert_eq!(
            parameters_preview(&same, &eligible).as_deref(),
            Some("-Verbose")
        );

        assert_eq!(parameters_preview(&BTreeMap::new(), &eligible), None);
    }

    #[test]
    fn action_detail_names_what_was_dispatched() {
        let mut req = request(ActionKind::Script, vec![1]);
        req.script_name = Some("Install-CriticalSecurityUpdates".into());
        assert_eq!(action_detail(&req), "Install-CriticalSecurityUpdates");

        req.script_name = None;
        req.script_id = Some(42);
        assert_eq!(action_detail(&req), "Script #42");

        let mut reboot = request(ActionKind::Reboot, vec![1]);
        reboot.reboot_mode = Some(RebootMode::Forced);
        assert_eq!(action_detail(&reboot), "Reboot (FORCED)");
        // The Jobs tab and the audit log must record which of the two applies ran —
        // "Apply OS patches" was ambiguous between them.
        assert_eq!(
            action_detail(&request(ActionKind::OsPatchApply, vec![1])),
            "Apply all OS patches"
        );
        assert_eq!(
            action_detail(&request(ActionKind::OsPatchRemediate, vec![1])),
            "Apply selected OS patches"
        );
    }
}
