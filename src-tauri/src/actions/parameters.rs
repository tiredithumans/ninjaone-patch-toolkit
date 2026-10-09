//! The `parameters` string a remediation script is sent, encoded by kind.

use super::*;

/// Builds the `parameters` string NinjaOne forwards to a library script.
///
/// NinjaOne splits `parameters` on **spaces** into `key=value` tokens, so a target
/// list whose entries contain spaces (third-party product titles like
/// "Google Chrome") cannot be sent literally. OS patches are KB numbers and are
/// safe as a bare comma list; software targets are base64-encoded into a single
/// space-free token that the script decodes and splits on `|`.
///
/// The software arm is reached via [`ActionKind::SoftwarePatchRemediate`]. Before
/// that kind existed it was **dead code**: the only caller composed parameters for
/// `ActionKind::Script`, which falls to the `kbAllowList` arm, so a software
/// remediation script was handed a KB list — and third-party patches carry no KB at
/// all, so the list was always empty.
///
/// The reference scripts in `remediation/` parse this string; the shared fixture
/// `remediation/tests/fixtures/parameter-contract.json` pins it on both sides.
pub fn build_parameters(
    kind: ActionKind,
    targets: &[String],
    reboot: RebootChoice,
    dry_run: bool,
) -> String {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let reboot = reboot.script_value();
    if uses_kb_encoding(kind) {
        // `plan()` blocks anything that is not a KB number; dropping it here as well
        // means a target can never reach the unquoted, space-split string even if a
        // new caller skips the planner.
        let kbs = targets
            .iter()
            .filter_map(|k| kb_number(k))
            .collect::<Vec<_>>()
            .join(",");
        format!("kbAllowList={kbs} rebootBehavior={reboot} dryRun={dry_run}")
    } else {
        let encoded = STANDARD.encode(targets.join("|"));
        format!("productAllowListB64={encoded} rebootBehavior={reboot} dryRun={dry_run}")
    }
}

/// Whether [`build_parameters`] sends this kind's targets as a bare `kbAllowList`
/// rather than base64-encoded product titles. Everything but the software family —
/// including a hand-picked `Script`, whose per-KB targeting is OS-only.
pub(super) fn uses_kb_encoding(kind: ActionKind) -> bool {
    !matches!(
        kind,
        ActionKind::SoftwarePatchApply
            | ActionKind::SoftwarePatchScan
            | ActionKind::SoftwarePatchRemediate
    )
}

/// The digits of a KB target — `KB5040434`, `kb5040434` or `5040434` — or `None`
/// for anything else, which must not be spliced into a space-split parameter string.
pub(super) fn kb_number(target: &str) -> Option<&str> {
    let t = target.trim();
    let digits = match t.get(..2) {
        Some(prefix) if prefix.eq_ignore_ascii_case("kb") => &t[2..],
        _ => t,
    };
    (!digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())).then_some(digits)
}
