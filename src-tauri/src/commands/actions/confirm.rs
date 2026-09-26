//! The confirmation fingerprint: what a `plan_action` token is bound to.

use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::Rng;
use sha2::{Digest, Sha256};

use super::ActionRequest;
use crate::api::actions::ScriptRef;

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
        reason: _,
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
    field(&[u8::from(*include_offline)]);
    field(&[u8::from(*override_window)]);
    field(&[u8::from(*dry_run)]);
    hex(&hasher.finalize())
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
