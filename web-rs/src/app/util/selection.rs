//! The per-row selection model and everything that turns a selection into an
//! `ActionRequest`: per-device targets, remediation summaries, the disabled /
//! blocked reasons the action bar shows, and the confirm-dialog gate.

use std::collections::{BTreeMap, BTreeSet};

use crate::types::{ActionKind, ActionRequest, AuthStatus, PatchRow, RebootChoice, RebootMode};

use super::super::state::{DeviceSelection, SelectedPatch};
use super::*;

/// Applies one row's checkbox to the selection map.
///
/// Pure map surgery, lifted out of the signal closure so it can be tested: this is
/// the load-bearing half of the selection model. A device enters the selection with
/// its first ticked row and leaves with its last, and ticking one row must affect
/// **only** that row — an earlier shape swept every KB on the device into the
/// selection, which made the one path capable of per-patch targeting unable to
/// receive a subset.
pub(crate) fn apply_row_selection(
    sel: &mut BTreeMap<i64, DeviceSelection>,
    row: &PatchRow,
    checked: bool,
) {
    let key = patch_key(row);
    if checked {
        sel.entry(row.device_id)
            .or_insert_with(|| DeviceSelection {
                name: row.device_name.clone(),
                organization: row.organization.clone(),
                offline: row.offline,
                patches: BTreeMap::new(),
            })
            .patches
            .insert(
                key,
                SelectedPatch {
                    kb: row.kb.clone().filter(|k| !k.is_empty()),
                    name: row.name.clone(),
                    is_os: row.patch_type.eq_ignore_ascii_case("OS"),
                },
            );
    } else if let Some(entry) = sel.get_mut(&row.device_id) {
        entry.patches.remove(&key);
        // The device leaves with its last ticked row, so a device with nothing
        // ticked is never dispatched against.
        if entry.patches.is_empty() {
            sel.remove(&row.device_id);
        }
    }
}

/// Identity of a patch *within a device's selection*.
///
/// The same `(patch_type, kb, name)` tuple the backend groups patches by, so a row
/// ticked in the flat view and the same patch ticked inside a grouped view refer to
/// one thing. Joined with a unit separator, which can't occur in a patch name, so
/// two distinct patches can never produce the same key.
pub(crate) fn patch_key(row: &PatchRow) -> String {
    format!(
        "{}\u{1f}{}\u{1f}{}",
        row.patch_type,
        row.kb.as_deref().unwrap_or(""),
        row.name
    )
}

/// The per-device target list a script dispatch would send, keyed by device id.
///
/// Devices with nothing ticked *of that family* are omitted entirely rather than
/// mapped to an empty list — for a remediation action they are not dispatched to at
/// all, so the confirmation dialog's device count is the number of devices that will
/// actually install something. Third-party patches carry no KB, so an OS target list
/// silently skips them and vice versa; that is the same asymmetry the two NinjaOne
/// feeds have.
///
/// Every path that sends an allow list goes through this — the two remediation kinds
/// and the hand-picked script's "Target only the selected KBs". Nothing hands a
/// device the batch-wide union of the selection any more.
pub(crate) fn targets_by_device(
    selected: &BTreeMap<i64, DeviceSelection>,
    want_os: bool,
) -> BTreeMap<i64, Vec<String>> {
    selected
        .iter()
        .filter_map(|(id, device)| {
            let targets: Vec<String> = device
                .patches
                .values()
                .filter(|p| p.is_os == want_os)
                // OS patches are targeted by KB, software by product title.
                .filter_map(|p| {
                    if want_os {
                        p.kb.clone()
                    } else {
                        Some(p.name.clone())
                    }
                })
                .filter(|t| !t.trim().is_empty())
                // The same patch can appear on a device via two rows (an install
                // attempt and the pending record); the allow list wants it once.
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            (!targets.is_empty()).then_some((*id, targets))
        })
        .collect()
}

/// [`targets_by_device`] for a remediation kind, which picks the family. Empty for
/// any other kind — the native endpoints take no target list at all.
/// The run options the ActionBar renders once and every dispatch reads.
///
/// Plain data, read out of the signals by the caller. `build_action_request` is then
/// pure and host-testable — which is the whole point: the branching below decides
/// which devices are dispatched to and what each is told to install, and it lived
/// inside an `AppState` method in a file with no test module at all, in a crate whose
/// only gates are a compile check and clippy.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RunOptions {
    pub use_kb_targeting: bool,
    pub include_offline: bool,
    pub override_window: bool,
    pub dry_run: bool,
    pub script_reboot: RebootChoice,
    pub run_as: String,
    pub reboot_mode_forced: bool,
    pub reason: String,
    pub script_id: Option<i64>,
    pub script_name: Option<String>,
    pub script_params: String,
}

/// Assembles the dispatch for `kind` from the current selection and run options.
///
/// Two rules here are load-bearing and are what the tests pin:
///
/// A remediation kind dispatches only to devices with a ticked patch **of its own
/// family**. Sending it the whole selection would hand a device with only software
/// rows ticked an empty OS allow list — a job that reports success having installed
/// nothing. A hand-picked script keeps every selected device, because the operator
/// chose them and the script may do something useful without a target list;
/// `plan()` warns about the ones that get an empty one.
///
/// The three shared run options reach every `runs_a_script()` kind and no other. The
/// native endpoints take no parameters, have no preview mode (a dry run of one is a
/// `plan()` blocker) and run as NinjaOne's agent, so setting them there would claim
/// protection those endpoints cannot give.
pub(crate) fn build_action_request(
    kind: ActionKind,
    selected: &BTreeMap<i64, DeviceSelection>,
    opts: &RunOptions,
) -> ActionRequest {
    // Which patches each device is told to install. A remediation kind takes its own
    // family; the hand-picked script path takes KBs, which is what its "Target only
    // the selected KBs" checkbox means and all a `kbAllowList` script can accept.
    let device_targets = if kind.is_remediation() {
        remediation_targets(selected, kind)
    } else if kind == ActionKind::Script && opts.use_kb_targeting {
        targets_by_device(selected, true)
    } else {
        BTreeMap::new()
    };

    let device_ids: Vec<i64> = if kind.is_remediation() {
        device_targets.keys().copied().collect()
    } else {
        selected.keys().copied().collect()
    };

    let mut req = ActionRequest::new(kind, device_ids);
    req.device_targets = device_targets;
    req.include_offline = opts.include_offline;
    req.override_window = opts.override_window;

    if kind.runs_a_script() {
        req.dry_run = opts.dry_run;
        req.reboot = opts.script_reboot;
        req.run_as = (!opts.run_as.trim().is_empty()).then(|| opts.run_as.clone());
    }

    if kind == ActionKind::Reboot {
        req.reboot_mode = Some(if opts.reboot_mode_forced {
            RebootMode::Forced
        } else {
            RebootMode::Normal
        });
        req.reason = Some(opts.reason.clone());
    }
    if kind == ActionKind::Script {
        req.script_id = opts.script_id;
        req.script_name = opts.script_name.clone();
        req.parameters =
            (!opts.script_params.trim().is_empty()).then(|| opts.script_params.clone());
    }
    req
}

pub(crate) fn remediation_targets(
    selected: &BTreeMap<i64, DeviceSelection>,
    kind: ActionKind,
) -> BTreeMap<i64, Vec<String>> {
    if !kind.is_remediation() {
        return BTreeMap::new();
    }
    targets_by_device(selected, kind.is_os_family())
}

/// What "install only the selected patches" would send, in one line per family.
///
/// `None` when the family has nothing ticked, so a selection of pure OS patches
/// doesn't render an empty "Software: 0" next to it.
pub(crate) fn remediation_summary(
    kind: ActionKind,
    targets: &BTreeMap<i64, Vec<String>>,
) -> Option<String> {
    if targets.is_empty() {
        return None;
    }
    let family = if kind.is_os_family() {
        "OS"
    } else {
        "Software"
    };
    // Distinct patches, not the sum of per-device lists: the same KB ticked on ten
    // devices is one patch going to ten devices, and reporting "10 patches" for it
    // is the batch-wide reading this whole path exists to replace.
    let distinct: BTreeSet<&String> = targets.values().flatten().collect();
    Some(format!(
        "{family}: {} patch(es) on {} device(s)",
        distinct.len(),
        targets.len()
    ))
}

/// What the script picker's "Target only the selected KBs" would actually send.
///
/// Spells out *per device* because that is the whole correction: the checkbox used to
/// hand every device the combined list from the entire selection.
pub(crate) fn kb_targeting_summary(targets: &BTreeMap<i64, Vec<String>>) -> String {
    if targets.is_empty() {
        return "No KBs selected — every device would be sent an empty allow list.".to_string();
    }
    let distinct: BTreeSet<&String> = targets.values().flatten().collect();
    format!(
        "Each device is sent only its own KBs — {} distinct KB(s) across {} device(s).",
        distinct.len(),
        targets.len()
    )
}

/// Why a specific action is unavailable, beyond the reasons that block all of them.
///
/// Separate from [`action_disabled_reason`] because these depend on the kind and on
/// what is ticked: the remediation actions need a configured script *and* a ticked
/// patch of their own family, and an operator who has ticked only software rows
/// needs to be told that, not left with a button that looks broken.
pub(crate) fn kind_disabled_reason(
    kind: ActionKind,
    script_configured: bool,
    matching_targets: usize,
) -> Option<String> {
    if !kind.is_remediation() {
        return None;
    }
    let family = if kind.is_os_family() {
        "OS"
    } else {
        "software"
    };
    if !script_configured {
        return Some(format!(
            "No {family} remediation script configured. NinjaOne has no per-patch apply endpoint, \
             so installing specific patches needs a library script that accepts a target list — \
             add its id in Settings → Patch actions."
        ));
    }
    if matching_targets == 0 {
        return Some(format!(
            "No {family} patches selected. Tick the {family} patch rows to install."
        ));
    }
    None
}

/// Why the patch-action affordances are unavailable, or `None` when they're live.
///
/// Pure and derived on demand rather than cached in a signal: it was previously
/// stored and recomputed by hand at two call sites, so signing in (or enabling
/// actions in Settings) left the startup verdict — "Sign in to run patch actions."
/// — on screen until the app was restarted. Deriving it means every input change
/// is picked up for free. The backend re-checks all of this; this only decides
/// what the UI offers.
///
/// `auth` is `None` before the first `auth_status` reply, which is treated as
/// not-yet-signed-in so the controls read as blocked while we don't know, rather
/// than briefly offering actions that would be rejected.
pub(crate) fn action_blocked_reason(
    web_mode: bool,
    demo: bool,
    auth: Option<&AuthStatus>,
) -> Option<String> {
    if web_mode || demo {
        return Some("Patch actions run only in the desktop app.".to_string());
    }
    let Some(status) = auth.filter(|a| a.authenticated) else {
        return Some("Sign in to run patch actions.".to_string());
    };
    if !status.actions_enabled {
        return Some("Patch actions are disabled — enable them in Settings.".to_string());
    }
    if !status.write_enabled {
        // Distinguish "we know it's read-only" from "we can't tell", so the
        // operator isn't told their consent was wrong when it may be fine.
        return Some(if status.scope_known {
            "Your NinjaOne sign-in is read-only. Re-authorize to enable actions.".to_string()
        } else {
            "Couldn't confirm your sign-in grants the Management scope. Re-authorize to be sure."
                .to_string()
        });
    }
    None
}

/// Why every action button is disabled, or `None` when they are live.
///
/// The precedence is the point and is why this is not inline: the tooltip must
/// name the most *fundamental* obstacle, so "sign in" outranks "select a device",
/// which outranks "an action is already running". Reordering these silently tells
/// an operator to pick devices when the real problem is that they are signed out.
/// `blocked` is [`action_blocked_reason`]'s verdict.
pub(crate) fn action_disabled_reason(
    blocked: Option<String>,
    selected_devices: usize,
    dispatching: bool,
) -> Option<String> {
    if blocked.is_some() {
        return blocked;
    }
    if selected_devices == 0 {
        return Some("Select at least one device first".to_string());
    }
    if dispatching {
        return Some("An action is already being dispatched".to_string());
    }
    None
}

/// The action bar's selection summary. `None` when nothing is selected, so the
/// caller renders the hint instead.
///
/// The offline clause is load-bearing for the operator's expectations: an action
/// against an offline device is *queued*, not run, so the count has to be visible
/// before they confirm — but only when it is non-zero, or every selection carries
/// a distracting "0 offline".
pub(crate) fn selection_summary(devices: usize, rows: usize, offline: usize) -> Option<String> {
    if devices == 0 {
        return None;
    }
    let mut text = format!(
        "{} device(s) selected · {} patch row(s)",
        group_thousands(devices),
        group_thousands(rows),
    );
    if offline > 0 {
        text.push_str(&format!(" · {offline} offline"));
    }
    Some(text)
}

/// Whether the confirm dialog must demand a typed device count rather than a
/// single click. A forced reboot is the one action here that destroys unsaved work
/// on machines the operator may not own, so it is deliberately harder to fire.
///
/// A blocked plan never needs it: the dispatch button is disabled anyway, and
/// asking someone to type a count that cannot be submitted reads as a bug.
pub(crate) fn needs_typed_confirmation(
    blocked: bool,
    kind: ActionKind,
    reboot_mode: Option<RebootMode>,
) -> bool {
    !blocked && kind == ActionKind::Reboot && reboot_mode == Some(RebootMode::Forced)
}

/// Whether the confirm button may fire. Extracted from the modal body so the rule
/// that guards the destructive path is host-testable rather than only reachable by
/// clicking through a browser.
pub(crate) fn can_confirm_action(
    blocked: bool,
    dispatching: bool,
    needs_typed: bool,
    typed: &str,
    expected: &str,
) -> bool {
    !blocked && !dispatching && (!needs_typed || typed.trim() == expected)
}
