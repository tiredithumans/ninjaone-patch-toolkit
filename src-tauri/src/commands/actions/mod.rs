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
use std::sync::Arc;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tracing::info;

use crate::actions::{ActionKind, ActionPlan, JobReport, JobState, RebootChoice, fmt_ts};
use crate::error::UiError;
use crate::model::{AutomationScript, RebootMode};
use crate::state::AppState;

mod confirm;
mod dispatch;
mod plan;
mod poller;

use confirm::random_token;
use dispatch::{DispatchContext, action_detail, dispatch_batch, invalidate_after};
use plan::{PlannedAction, build_plan};
use poller::spawn_job_poller;

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
    /// confirmation dialog. Honored only for `Script` (see `typed_parameters`); when
    /// absent, each device's string is composed from its `device_targets` +
    /// `reboot` + `dry_run`.
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

/// Reports what an action would do, without doing it.
#[tauri::command]
pub async fn plan_action(
    state: State<'_, AppState>,
    request: ActionRequest,
) -> Result<ActionPlan, UiError> {
    require_actions_enabled(&state)?;
    let planned = build_plan(&state, &request).await?;
    let hash = planned.hash(&request);
    let mut plan = planned.plan;

    // A blocked plan has nothing to confirm, so it gets no token. Scans are not
    // mutating and skip confirmation entirely.
    if !plan.is_blocked() && request.kind.is_mutating() {
        let token = random_token();
        state.store_pending_confirm(token.clone(), hash);
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
    let planned = build_plan(&state, &request).await?;
    if planned.plan.is_blocked() {
        return Err(UiError::new(planned.plan.blockers.join(" ")));
    }

    if request.kind.is_mutating() {
        let token = request.confirm_token.as_deref().unwrap_or_default();
        if !state.consume_confirm_token(token, &planned.hash(&request)) {
            return Err(UiError::new(
                "This action was not confirmed, or the confirmation expired or no longer matches \
                 the selection. Review the plan and confirm again.",
            ));
        }
    }
    let PlannedAction {
        plan: p,
        parameters,
        script,
        run_as,
    } = planned;

    let settings = state.settings_snapshot();
    // The identity the approval was bound to — never re-read from Settings here. The
    // native endpoints take none.
    let run_as = run_as.unwrap_or_default();
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

    if live > 0 {
        invalidate_after(request.kind, request.dry_run, &state);
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
mod tests;
