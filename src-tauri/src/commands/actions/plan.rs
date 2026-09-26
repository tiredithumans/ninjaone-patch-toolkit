//! Planning: resolves a request down to exactly what would be dispatched, so
//! `plan_action` and `run_action` share one answer.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::Local;

use super::ActionRequest;
use super::confirm::{canonical_parameters, request_hash};
use crate::actions::{ActionKind, ActionPlan, PlanInput, PlannedTarget, plan};
use crate::api::actions::ScriptRef;
use crate::error::UiError;
use crate::settings::ActionSettings;
use crate::state::AppState;

/// The `parameters` string sent to each device, keyed by device id.
///
/// `BTreeMap` rather than `HashMap` so the canonical rendering below — and thus the
/// confirmation hash — does not depend on iteration order.
///
/// Three shapes, one function, because they must agree: `Script` sends one
/// hand-composed string to every device; a remediation kind composes a *distinct*
/// string per device from that device's ticked patches; a native endpoint takes no
/// parameters at all.
pub(super) fn per_device_parameters(req: &ActionRequest) -> BTreeMap<i64, String> {
    if !req.kind.runs_a_script() {
        return BTreeMap::new();
    }
    if let Some(verbatim) = typed_parameters(req) {
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

/// The hand-typed `parameters` string, if this request sends one.
///
/// A hand-written string is batch-wide by nature and is sent verbatim — the toolkit
/// never rewrites what the operator typed. Only `Script` honors one: the remediation
/// kinds have no field to type it in, and honoring one there would silently discard
/// the per-device targeting that is their entire purpose.
fn typed_parameters(req: &ActionRequest) -> Option<&str> {
    if req.kind != ActionKind::Script {
        return None;
    }
    req.parameters
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
}

/// The targets [`per_device_parameters`] will compose into parameter strings — only
/// those of devices in the request, and none when nothing is composed (a native
/// endpoint, or a typed string sent verbatim). This is what `plan()` checks, so the
/// guardrails see exactly the targets that reach NinjaOne.
pub(super) fn composed_targets(req: &ActionRequest) -> Vec<&str> {
    if !req.kind.runs_a_script() || typed_parameters(req).is_some() {
        return Vec::new();
    }
    // Only the targets belonging to devices actually in this request count — a
    // frontend that left stale entries in the map must not satisfy the "something is
    // selected" guardrail with patches for devices it is not dispatching to.
    req.device_ids
        .iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter_map(|id| req.device_targets.get(id))
        .flatten()
        .map(String::as_str)
        .collect()
}

/// The execution identity a script-running request is dispatched with, `None` for
/// the native endpoints (they run as NinjaOne's agent). A blank Run-as means the
/// Settings default, resolved here once so the confirmation binds and the dispatch
/// sends the same value — `run_action` used to re-read Settings after the token
/// check, so the default could change under an approval.
pub(super) fn resolve_run_as(req: &ActionRequest, settings: &ActionSettings) -> Option<String> {
    req.kind.runs_a_script().then(|| {
        req.run_as
            .clone()
            .filter(|r| !r.trim().is_empty())
            .unwrap_or_else(|| settings.run_as.clone())
    })
}

/// What the operator is shown in the confirmation dialog.
///
/// One line per device when the strings differ (remediation), the bare string when
/// they don't (a hand-driven script) — the toolkit never sends a `parameters` string
/// the operator has not seen, and with per-device targeting that means all of them.
pub(super) fn parameters_preview(
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
pub(super) fn untargeted_names<'a>(
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
pub(super) fn summarize_names(names: &[&str]) -> String {
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
fn resolve_script(req: &ActionRequest, settings: &ActionSettings) -> Option<ScriptRef> {
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
pub(super) struct PlannedAction {
    pub(super) plan: ActionPlan,
    /// Device id → the `parameters` string that device will be sent.
    pub(super) parameters: BTreeMap<i64, String>,
    pub(super) script: Option<ScriptRef>,
    /// The identity sent as `runAs`, after defaulting a blank request to Settings.
    /// `None` for the native endpoints.
    pub(super) run_as: Option<String>,
}

impl PlannedAction {
    /// The confirmation fingerprint of `req` as planned here.
    pub(super) fn hash(&self, req: &ActionRequest) -> String {
        request_hash(
            req,
            &canonical_parameters(&self.parameters),
            self.script.as_ref(),
            self.run_as.as_deref(),
        )
    }
}

/// Shared planning path for `plan_action` and `run_action`, so the two can never
/// disagree about what the guardrails say.
pub(super) async fn build_plan(
    state: &AppState,
    req: &ActionRequest,
) -> Result<PlannedAction, UiError> {
    let settings = state.settings_snapshot().actions;
    // Borrowed out of the warm caches: this runs on every plan *and* every confirm,
    // and used to deep-clone the whole fleet (and all three lookup lists) to read it.
    let devices = state.fleet_devices(None).await.map_err(UiError::from)?;
    let org_names = state.org_names().await.map_err(UiError::from)?;

    let targets = composed_targets(req);
    let mut p = plan(PlanInput {
        kind: req.kind,
        device_ids: &req.device_ids,
        devices: &devices,
        org_names: &org_names,
        settings: &settings,
        include_offline: req.include_offline,
        override_window: req.override_window,
        reboot_mode: req.reboot_mode,
        reboot: req.reboot,
        dry_run: req.dry_run,
        targets: &targets,
        now: Local::now(),
    });

    let parameters = per_device_parameters(req);
    p.parameters_preview = parameters_preview(&parameters, &p.eligible);
    let script = resolve_script(req, &settings);
    let run_as = resolve_run_as(req, &settings);

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
        run_as,
    })
}
