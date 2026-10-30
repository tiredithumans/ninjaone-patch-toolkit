//! The confirmation fingerprint: what a `plan_action` token is bound to, and the
//! check `run_action` spends it on.

use std::collections::{BTreeMap, BTreeSet};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use sha2::{Digest, Sha256};

use super::ActionRequest;
use super::plan::PlannedAction;
use crate::actions::{ActionPlan, ConfirmCheck, summarize_names};
use crate::api::actions::ScriptRef;
use crate::error::UiError;
use crate::state::AppState;

/// Stable fingerprint of everything that determines what would be dispatched **and
/// everything the guardrails react to**.
///
/// A confirmation token is only honored alongside a matching hash, so editing the
/// device list (or the parameters, the reboot mode and reason, the run-as identity,
/// or either guardrail toggle) after the dialog opened invalidates the approval instead of
/// silently widening it.
///
/// `eligible` is the plan's eligible device ids, the devices the dialog said it
/// would run on. The request's ids are not enough: which of them are eligible
/// depends on state the request does not carry (online status, the inventory,
/// Settings), and `run_action` dispatches to whatever its re-plan finds eligible.
/// Bound to the request alone, an approval reviewed with a device skipped as offline
/// dispatched to it if it came back online before Run was pressed.
///
/// The request is destructured exhaustively so that adding a field to
/// `ActionRequest` fails to compile here rather than silently falling outside the
/// binding — which is exactly how `include_offline`, `override_window` and `run_as`
/// came to be missing. The first two are the flags `plan()` uses to gate the
/// offline-queue warning and the maintenance-window blocker, so a token issued
/// under one blast radius validated under a wider one; `run_as` selects the
/// execution identity sent to NinjaOne, so an approval for `system` validated after
/// being switched to a stored credential.
///
/// Where the request and what is dispatched can differ, the *resolved* value is
/// hashed: the script (a remediation kind's comes from Settings) and the run-as
/// identity (a blank request means the Settings default — hashing the request's
/// blank let the default change between review and confirm under one approval).
pub(super) fn request_hash(
    req: &ActionRequest,
    parameters: &str,
    script: Option<&ScriptRef>,
    run_as: Option<&str>,
    eligible: &[i64],
) -> String {
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
        // Hashed as resolved, via the `run_as` argument.
        run_as: _,
        reboot,
        reboot_mode,
        reason,
        include_offline,
        override_window,
        dry_run,
        // The token being validated cannot be part of its own fingerprint.
        confirm_token: _,
    } = req;

    // Sorted, but *not* de-duplicated: a repeated id is a second dispatch to that
    // device, so `[5, 5]` must not hash like `[5]`. (`plan()` blocks a repeat
    // outright; this keeps the hash from ever vouching for one.)
    let mut ids = device_ids.clone();
    ids.sort_unstable();
    let mut eligible = eligible.to_vec();
    eligible.sort_unstable();

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
    // Length-prefixed so `None` (a native endpoint, which sends no identity) and an
    // empty resolved default cannot hash alike.
    field(
        run_as
            .map(|r| format!("{}:{r}", r.len()))
            .unwrap_or_default()
            .as_bytes(),
    );
    field(format!("{reboot:?}").as_bytes());
    field(format!("{reboot_mode:?}").as_bytes());
    // The reboot reason is sent to NinjaOne and lands in its activity feed as the
    // server-side record of why the machine went down, so the approval covers the
    // text the operator reviewed. It was excluded as if it were display-only. Hashed
    // as dispatched (`None` sends an empty string) and length-prefixed, because it is
    // free text typed by hand and may contain the separator byte.
    let reason = reason.as_deref().unwrap_or_default();
    field(format!("{}:{reason}", reason.len()).as_bytes());
    field(&[u8::from(*include_offline)]);
    field(&[u8::from(*override_window)]);
    field(&[u8::from(*dry_run)]);
    field(
        eligible
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(",")
            .as_bytes(),
    );
    hex(&hasher.finalize())
}

/// What `run_action` says when the token does not authorize its re-plan.
const NOT_CONFIRMED: &str = "This action was not confirmed, or the confirmation expired or no \
     longer matches the selection. Review the plan and confirm again.";

/// Spends the request's confirmation token on `planned`, the re-plan `run_action`
/// would dispatch, or says why the token does not authorize it. A scan is not
/// confirmed at all, so it passes.
pub(super) fn confirm_dispatch(
    state: &AppState,
    request: &ActionRequest,
    planned: &PlannedAction,
) -> Result<(), UiError> {
    if !request.kind.is_mutating() {
        return Ok(());
    }
    let token = request.confirm_token.as_deref().unwrap_or_default();
    match state.take_confirmation(token, &planned.hash(request), &planned.plan.eligible_ids()) {
        ConfirmCheck::Confirmed => Ok(()),
        ConfirmCheck::TargetsChanged { reviewed } => {
            Err(UiError::new(targets_changed(&reviewed, &planned.plan)))
        }
        ConfirmCheck::Refused => Err(UiError::new(NOT_CONFIRMED)),
    }
}

/// The refusal for an approval whose devices moved between review and Run: which
/// ones the re-plan would add and which it would drop, by name.
pub(super) fn targets_changed(reviewed: &[i64], plan: &ActionPlan) -> String {
    let reviewed: BTreeSet<i64> = reviewed.iter().copied().collect();
    let now: BTreeSet<i64> = plan.eligible.iter().map(|t| t.device_id).collect();
    let added: Vec<&str> = plan
        .eligible
        .iter()
        .filter(|t| !reviewed.contains(&t.device_id))
        .map(|t| t.device_name.as_str())
        .collect();
    // A device that dropped out is in the re-plan's skipped list with its name; one
    // that is not (an id the inventory no longer lists) is named by id.
    let dropped: Vec<String> = reviewed
        .difference(&now)
        .map(|id| {
            plan.skipped
                .iter()
                .find(|s| s.device_id == *id)
                .map_or_else(|| format!("Device {id}"), |s| s.device_name.clone())
        })
        .collect();
    let dropped: Vec<&str> = dropped.iter().map(String::as_str).collect();
    let mut moved = Vec::new();
    if !added.is_empty() {
        moved.push(format!("now also {}", summarize_names(&added)));
    }
    if !dropped.is_empty() {
        moved.push(format!("no longer {}", summarize_names(&dropped)));
    }
    format!(
        "Nothing was sent: the devices this action would run on changed since you reviewed it \
         ({}). A device may have come back online or gone offline. Review the plan again.",
        moved.join("; ")
    )
}

fn hex(bytes: &[u8]) -> String {
    // Written directly rather than `format!` per byte, which allocated and dropped a
    // `String` for each of the digest's 32 bytes on every plan and every confirm.
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

pub(super) fn random_token() -> String {
    let mut bytes = [0u8; 24];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
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
pub(super) fn canonical_parameters(params: &BTreeMap<i64, String>) -> String {
    params
        .iter()
        .map(|(id, p)| format!("{id}:{}:{p}", p.len()))
        .collect::<Vec<_>>()
        .join("\u{1e}")
}
