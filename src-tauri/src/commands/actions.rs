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

use std::collections::HashMap;

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
    ActionKind, ActionPlan, JobReport, JobState, PlanInput, RebootChoice, audit, fmt_ts, plan,
};
use crate::api::actions::ScriptRef;
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
    /// list. Ignored by the native endpoints, which have no per-patch variant.
    #[serde(default)]
    pub targets: Vec<String>,
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
fn request_hash(req: &ActionRequest, parameters: &str) -> String {
    let ActionRequest {
        kind,
        device_ids,
        targets,
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
    // targets ["a","b"] vs ["a\u{1f}b"]).
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
    field(targets.join("\u{1f}").as_bytes());
    field(&script_id.unwrap_or_default().to_le_bytes());
    field(script_uid.as_deref().unwrap_or_default().as_bytes());
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

/// The `parameters` string this request will send. Composed from the targets when
/// the operator hasn't written one by hand.
fn effective_parameters(req: &ActionRequest) -> Option<String> {
    if req.kind != ActionKind::Script {
        return None;
    }
    Some(match req.parameters.as_ref().map(|p| p.trim()) {
        Some(p) if !p.is_empty() => p.to_string(),
        _ => crate::actions::build_parameters(req.kind, &req.targets, req.reboot, req.dry_run),
    })
}

/// Shared planning path for `plan_action` and `run_action`, so the two can never
/// disagree about what the guardrails say.
async fn build_plan(state: &AppState, req: &ActionRequest) -> Result<ActionPlan, UiError> {
    let settings = state.settings_snapshot();
    let devices: Vec<Device> = state
        .fleet_devices(None)
        .await
        .map_err(UiError::from)?
        .as_ref()
        .clone();
    let (orgs, _, _) = state.lookups().await.map_err(UiError::from)?;
    let org_names: HashMap<i64, String> = orgs.iter().map(|o| (o.id, o.name.clone())).collect();

    let mut p = plan(PlanInput {
        kind: req.kind,
        device_ids: &req.device_ids,
        devices: &devices,
        org_names: &org_names,
        settings: &settings.actions,
        include_offline: req.include_offline,
        override_window: req.override_window,
        reboot_mode: req.reboot_mode,
        dry_run: req.dry_run,
        now: Local::now(),
    });

    p.parameters_preview = effective_parameters(req);

    // Request-shape problems the pure planner can't see, since they depend on
    // settings and on which script was picked.
    if req.kind == ActionKind::Script && req.script_id.is_none() && req.script_uid.is_none() {
        p.blockers
            .push("No script selected. Choose one from the automation library.".into());
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
    Ok(p)
}

/// Reports what an action would do, without doing it.
#[tauri::command]
pub async fn plan_action(
    state: State<'_, AppState>,
    request: ActionRequest,
) -> Result<ActionPlan, UiError> {
    require_actions_enabled(&state)?;
    let mut p = build_plan(&state, &request).await?;

    // A blocked plan has nothing to confirm, so it gets no token. Scans are not
    // mutating and skip confirmation entirely.
    if !p.is_blocked() && request.kind.is_mutating() {
        let params = p.parameters_preview.clone().unwrap_or_default();
        let token = random_token();
        state.store_pending_confirm(token.clone(), request_hash(&request, &params));
        p.confirm_token = Some(token);
    }
    Ok(p)
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
    let p = build_plan(&state, &request).await?;
    if p.is_blocked() {
        return Err(UiError::new(p.blockers.join(" ")));
    }

    let parameters = p.parameters_preview.clone().unwrap_or_default();
    if request.kind.is_mutating() {
        let token = request.confirm_token.as_deref().unwrap_or_default();
        if !state.consume_confirm_token(token, &request_hash(&request, &parameters)) {
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
    let script = match (request.script_id, request.script_uid.clone()) {
        (Some(id), _) => Some(ScriptRef::Script { id }),
        (None, Some(uid)) => Some(ScriptRef::Action { uid }),
        _ => None,
    };
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

    let permits = settings.actions.concurrency.clamp(1, 16);
    let sem = std::sync::Arc::new(Semaphore::new(permits));
    let mut set: JoinSet<(usize, JobReport)> = JoinSet::new();

    for (index, target) in p.eligible.iter().enumerate() {
        let api = state.api.clone();
        let sem = sem.clone();
        let kind = request.kind;
        let script = script.clone();
        let run_as = run_as.clone();
        let parameters = parameters.clone();
        let reason = request.reason.clone().unwrap_or_default();
        let reboot_mode = request.reboot_mode.unwrap_or(RebootMode::Normal);
        let dry_run = request.dry_run;
        let detail = detail.clone();
        let target = target.clone();
        let instance = settings.instance_base_url.clone();
        let client_id = settings.client_id.clone();
        let confirm_prefix = request
            .confirm_token
            .as_deref()
            .map(|t| t.chars().take(8).collect::<String>());

        set.spawn(async move {
            let dispatched_at = Utc::now();
            let mut job = JobReport {
                id: id_base + index as u64,
                batch_id,
                device_id: target.device_id,
                device_name: target.device_name.clone(),
                organization: target.organization.clone(),
                kind,
                detail: detail.clone(),
                dry_run,
                state: JobState::Queued,
                dispatched_at: fmt_ts(dispatched_at),
                dispatched_ts: dispatched_at.timestamp(),
                finished_at: None,
                duration_seconds: None,
                activity_id: None,
                series_uid: None,
                exit_code: None,
            };

            // Written before the request goes out, so a crash mid-batch still
            // leaves evidence of what was attempted.
            audit::record(&audit::AuditEntry {
                timestamp: audit::now_stamp(),
                instance,
                client_id,
                batch_id,
                job_id: job.id,
                kind,
                device_id: target.device_id,
                device_name: target.device_name.clone(),
                organization: target.organization.clone(),
                detail,
                parameters: (!parameters.is_empty()).then(|| audit::redact_parameters(&parameters)),
                dry_run,
                confirm_token_prefix: confirm_prefix,
                outcome: "dispatching".into(),
                activity_id: None,
                series_uid: None,
                exit_code: None,
            });

            let _permit = sem.acquire().await;
            // Defense in depth: `plan()` already blocks a dry run on a kind with no
            // preview mode, and `run_action` refuses a blocked plan — but that put
            // the "a dry run never mutates a device" property two files away from
            // the POSTs that would do the mutating, resting entirely on
            // `supports_dry_run()` being right. A new `ActionKind` that answers it
            // wrongly would otherwise dispatch for real while the UI said "Dry run".
            let outcome = if dry_run && !kind.supports_dry_run() {
                Err(anyhow::anyhow!(
                    "refusing to dispatch \"{}\" as a dry run: it has no preview mode",
                    kind.label()
                ))
            } else {
                match kind {
                    ActionKind::OsPatchScan => api
                        .device_patch_scan(target.device_id, PatchType::Os)
                        .await
                        .map(|_| None),
                    ActionKind::SoftwarePatchScan => api
                        .device_patch_scan(target.device_id, PatchType::Software)
                        .await
                        .map(|_| None),
                    ActionKind::OsPatchApply => api
                        .device_patch_apply(target.device_id, PatchType::Os)
                        .await
                        .map(|_| None),
                    ActionKind::SoftwarePatchApply => api
                        .device_patch_apply(target.device_id, PatchType::Software)
                        .await
                        .map(|_| None),
                    ActionKind::Reboot => api
                        .device_reboot(target.device_id, reboot_mode, &reason)
                        .await
                        .map(|_| None),
                    ActionKind::Script => match script {
                        Some(ref s) => api
                            .run_script(target.device_id, s, &parameters, &run_as)
                            .await
                            .map(Some),
                        None => Err(anyhow::anyhow!("no script selected")),
                    },
                }
            };

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
                    // A timed-out POST may already be queued on the device, so it
                    // is recorded as Unknown — polled, but never auto-retried.
                    job.state = if msg.contains("may already") {
                        JobState::Unknown(msg)
                    } else {
                        let failed = JobState::Failed(msg);
                        job.finish(failed.clone(), Utc::now());
                        failed
                    };
                }
            }
            (index, job)
        });
    }

    let mut dispatched: Vec<Option<JobReport>> = vec![None; p.eligible.len()];
    let mut done = 0usize;
    while let Some(res) = set.join_next().await {
        match res {
            Ok((index, job)) => {
                done += 1;
                emit_progress(
                    &app,
                    ActionProgressEvent {
                        batch_id,
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
    jobs.extend(dispatched.into_iter().flatten());

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

    fn request(kind: ActionKind, ids: Vec<i64>) -> ActionRequest {
        ActionRequest {
            kind,
            device_ids: ids,
            targets: Vec::new(),
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
        let h = request_hash(&base, "");

        // `plan()` gates the offline-queue warning on this.
        let mut offline = request(ActionKind::Reboot, vec![1, 2]);
        offline.include_offline = true;
        assert_ne!(h, request_hash(&offline, ""), "include_offline must bind");

        // ...and the maintenance-window blocker on this.
        let mut window = request(ActionKind::Reboot, vec![1, 2]);
        window.override_window = true;
        assert_ne!(h, request_hash(&window, ""), "override_window must bind");

        // `run_as` is sent to NinjaOne verbatim as the execution identity, so an
        // approval for `system` must not validate against a stored credential.
        let mut elevated = request(ActionKind::Reboot, vec![1, 2]);
        elevated.run_as = Some("domain-admin".into());
        assert_ne!(h, request_hash(&elevated, ""), "run_as must bind");

        let mut reboot = request(ActionKind::Reboot, vec![1, 2]);
        reboot.reboot = RebootChoice::Auto;
        assert_ne!(h, request_hash(&reboot, ""), "reboot choice must bind");

        let mut mode = request(ActionKind::Reboot, vec![1, 2]);
        mode.reboot_mode = Some(RebootMode::Forced);
        assert_ne!(h, request_hash(&mode, ""), "reboot mode must bind");

        let mut dry = request(ActionKind::Reboot, vec![1, 2]);
        dry.dry_run = true;
        assert_ne!(h, request_hash(&dry, ""), "dry_run must bind");

        let mut targets = request(ActionKind::Reboot, vec![1, 2]);
        targets.targets = vec!["KB5000001".into()];
        assert_ne!(h, request_hash(&targets, ""), "targets must bind");

        // The effective parameters are what actually go on the wire.
        assert_ne!(
            h,
            request_hash(&base, "dryRun=false"),
            "parameters must bind"
        );
    }

    /// Field values are separated, so two different requests cannot concatenate
    /// into the same hash input.
    #[test]
    fn request_hash_is_not_confusable_across_field_boundaries() {
        let mut a = request(ActionKind::Script, vec![1]);
        a.targets = vec!["ab".into()];
        a.script_uid = Some(String::new());

        let mut b = request(ActionKind::Script, vec![1]);
        b.targets = vec!["a".into(), "b".into()];
        b.script_uid = Some(String::new());
        assert_ne!(request_hash(&a, ""), request_hash(&b, ""));

        let mut c = request(ActionKind::Script, vec![1]);
        c.targets = vec!["x".into()];
        c.script_uid = Some("y".into());

        let mut d = request(ActionKind::Script, vec![1]);
        d.targets = vec!["xy".into()];
        d.script_uid = None;
        assert_ne!(request_hash(&c, ""), request_hash(&d, ""));
    }

    #[test]
    fn request_hash_ignores_device_order_but_not_membership() {
        let a = request(ActionKind::Reboot, vec![3, 1, 2]);
        let b = request(ActionKind::Reboot, vec![1, 2, 3]);
        assert_eq!(request_hash(&a, ""), request_hash(&b, ""));

        // Adding a device must invalidate an approval issued for the smaller set.
        let c = request(ActionKind::Reboot, vec![1, 2, 3, 4]);
        assert_ne!(request_hash(&a, ""), request_hash(&c, ""));
    }

    #[test]
    fn request_hash_covers_the_fields_that_change_what_happens() {
        let base = request(ActionKind::Script, vec![1]);
        let baseline = request_hash(&base, "kbAllowList=1 dryRun=true");

        assert_ne!(
            baseline,
            request_hash(&base, "kbAllowList=999 dryRun=false"),
            "different parameters must not reuse an approval"
        );
        assert_ne!(
            baseline,
            request_hash(
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
            request_hash(
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
            request_hash(
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
        let mut req = request(ActionKind::Script, vec![1]);
        req.targets = vec!["KB5040434".into()];
        assert_eq!(
            effective_parameters(&req).as_deref(),
            Some("kbAllowList=5040434 rebootBehavior=Never dryRun=false")
        );

        // A hand-written string is used verbatim — never silently rewritten.
        req.parameters = Some("  -Verbose  ".into());
        assert_eq!(effective_parameters(&req).as_deref(), Some("-Verbose"));

        // Native endpoints take no parameters at all.
        assert_eq!(
            effective_parameters(&request(ActionKind::Reboot, vec![1])),
            None
        );
        assert_eq!(
            effective_parameters(&request(ActionKind::OsPatchApply, vec![1])),
            None
        );
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
        assert_eq!(
            action_detail(&request(ActionKind::OsPatchApply, vec![1])),
            "Apply OS patches"
        );
    }
}
