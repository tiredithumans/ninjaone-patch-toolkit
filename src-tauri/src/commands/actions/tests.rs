use std::collections::{BTreeMap, HashMap};

use chrono::Utc;

use super::confirm::{canonical_parameters, request_hash};
use super::dispatch::{action_detail, invalidate_after, record_dispatch};
use super::plan::{
    composed_targets, parameters_preview, per_device_parameters, resolve_run_as, summarize_names,
    untargeted_names,
};
use super::*;
use crate::actions::{PlannedTarget, RebootChoice, fmt_ts};
use crate::api::actions::{ScriptDispatch, ScriptRef};
use crate::settings::ActionSettings;

/// `request_hash` with no resolved script, which is every case except the two
/// remediation kinds, and the request's own run-as taken as resolved.
fn hash(req: &ActionRequest, parameters: &str) -> String {
    request_hash(req, parameters, None, req.run_as.as_deref())
}

/// A blank Run-as resolves to the Settings default, and it is that resolved
/// value the approval binds: `run_action` used to fall back to Settings *after*
/// the token check, so the default could change between review and confirm
/// and dispatch under the old approval as a different identity.
#[test]
fn the_confirmation_binds_the_resolved_run_as_default() {
    let req = request(ActionKind::Script, vec![1]);
    let as_system = ActionSettings {
        run_as: "system".into(),
        ..ActionSettings::default()
    };
    let as_admin = ActionSettings {
        run_as: "domain-admin".into(),
        ..ActionSettings::default()
    };
    assert_eq!(resolve_run_as(&req, &as_system).as_deref(), Some("system"));
    let blank = ActionRequest {
        run_as: Some("  ".into()),
        ..req.clone()
    };
    assert_eq!(
        resolve_run_as(&blank, &as_admin).as_deref(),
        Some("domain-admin")
    );
    // An explicit choice wins over the default.
    let explicit = ActionRequest {
        run_as: Some("local-admin".into()),
        ..req.clone()
    };
    assert_eq!(
        resolve_run_as(&explicit, &as_admin).as_deref(),
        Some("local-admin")
    );

    let bound =
        |s: &ActionSettings| request_hash(&req, "", None, resolve_run_as(&req, s).as_deref());
    assert_ne!(
        bound(&as_system),
        bound(&as_admin),
        "a changed Settings default must invalidate an approval for a blank Run-as"
    );

    // The native endpoints run as NinjaOne's agent, so they carry no identity —
    // and none must hash like an empty default.
    assert_eq!(
        resolve_run_as(&request(ActionKind::Reboot, vec![1]), &as_system),
        None
    );
    assert_ne!(
        request_hash(&req, "", None, None),
        request_hash(&req, "", None, Some("")),
    );
}

/// The hash used to sort *and de-duplicate* the ids, so an approval for `[5]`
/// validated `[5, 5]` — a second run on the same machine.
#[test]
fn request_hash_does_not_collapse_a_repeated_device() {
    assert_ne!(
        hash(&request(ActionKind::Reboot, vec![5]), ""),
        hash(&request(ActionKind::Reboot, vec![5, 5]), "")
    );
}

/// Every ambiguous failure of an acting POST is `Unknown` (polled, never
/// replayed) — classified by type, not by a phrase only the timeout message
/// carried. A rejection is `Failed` and finished.
#[test]
fn a_dispatch_outcome_is_classified_by_the_error_type() {
    use crate::api::OutcomeUnknown;
    let now = Utc::now();
    let job = || JobReport {
        id: 1,
        batch_id: 1,
        device_id: 7,
        device_name: "srv-1".into(),
        organization: "Contoso".into(),
        kind: ActionKind::OsPatchApply,
        detail: "Apply all OS patches".into(),
        dry_run: false,
        state: JobState::Queued,
        dispatched_at: fmt_ts(now),
        dispatched_ts: now.timestamp(),
        finished_at: None,
        duration_seconds: None,
        activity_id: None,
        series_uid: None,
        exit_code: None,
    };

    let mut unknown = job();
    record_dispatch(
        &mut unknown,
        Err(
            anyhow::Error::new(OutcomeUnknown("NinjaOne answered 502".into()))
                .context("dispatching to srv-1"),
        ),
        now,
    );
    assert!(
        matches!(unknown.state, JobState::Unknown(_)),
        "{:?}",
        unknown.state
    );
    assert!(
        !unknown.state.is_terminal(),
        "an unknown job is still polled"
    );
    assert_eq!(unknown.finished_at, None);

    let mut rejected = job();
    record_dispatch(
        &mut rejected,
        Err(anyhow::anyhow!(
            "POST … failed (400 Bad Request): not applicable — the action may already be queued"
        )),
        now,
    );
    assert!(
        matches!(rejected.state, JobState::Failed(_)),
        "a rejection is not unknown however it is worded: {:?}",
        rejected.state
    );
    assert!(rejected.finished_at.is_some());

    let mut sent = job();
    record_dispatch(
        &mut sent,
        Ok(Some(ScriptDispatch {
            activity_id: Some(900),
            ..ScriptDispatch::default()
        })),
        now,
    );
    assert_eq!(sent.state, JobState::Running);
    assert_eq!(sent.activity_id, Some(900));
}

/// `plan()` validates exactly the targets that will be composed into a
/// parameter string: those of the requested devices, and none when the string is
/// typed by hand or the kind takes no parameters.
#[test]
fn composed_targets_are_what_reaches_the_parameter_string() {
    let mut req = request(ActionKind::OsPatchRemediate, vec![1, 2]);
    req.device_targets = HashMap::from([
        (1, vec!["KB1".into()]),
        (2, vec!["KB2".into(), "KB3".into()]),
        // Not in the request: must not count.
        (9, vec!["123 dryRun=false".into()]),
    ]);
    let mut got = composed_targets(&req);
    got.sort_unstable();
    assert_eq!(got, vec!["KB1", "KB2", "KB3"]);

    let native = ActionRequest {
        kind: ActionKind::OsPatchApply,
        ..req.clone()
    };
    assert!(composed_targets(&native).is_empty());

    let typed = ActionRequest {
        kind: ActionKind::Script,
        parameters: Some("-Verbose".into()),
        ..req.clone()
    };
    assert!(composed_targets(&typed).is_empty());
    let composed_script = ActionRequest {
        kind: ActionKind::Script,
        ..req
    };
    assert_eq!(composed_targets(&composed_script).len(), 3);
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

/// The reboot reason is sent to NinjaOne and recorded in its activity feed, so an
/// approval for one reason must not dispatch with another. It used to be excluded
/// from the hash as if it were display-only.
#[test]
fn request_hash_binds_the_reboot_reason() {
    let mut a = request(ActionKind::Reboot, vec![1]);
    a.reason = Some("Monthly patch window".into());
    let mut b = a.clone();
    b.reason = Some("Something else entirely".into());
    assert_ne!(hash(&a, ""), hash(&b, ""), "the reason must bind");

    // Hashed as dispatched: an absent reason is sent as an empty one.
    let mut none = a.clone();
    none.reason = None;
    let mut empty = a.clone();
    empty.reason = Some(String::new());
    assert_eq!(hash(&none, ""), hash(&empty, ""));
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
        request_hash(&a, &canon(&a), Some(&ScriptRef::Script { id: 42 }), None),
        request_hash(&a, &canon(&a), Some(&ScriptRef::Script { id: 43 }), None),
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
/// Every mutating `#[tauri::command]` in `mod.rs` must call
/// `require_actions_enabled`. The gate is a hand-placed call rather than
/// something the type system demands, so a new command compiles perfectly well
/// without it — and that file is the entire write path. Derived from the source
/// rather than an enumeration, so adding a command is what makes it fail.
///
/// The two exemptions are read-only over local job state and reach no device.
#[test]
fn every_mutating_command_checks_that_actions_are_enabled() {
    const READ_ONLY: [&str; 2] = ["list_jobs", "clear_jobs"];
    let src = include_str!("mod.rs");

    // Ignore the test module, which mentions both the attribute and the gate.
    let body = src
        .split("\n#[cfg(test)]")
        .next()
        .expect("source before tests");

    let mut checked = 0;
    for (idx, _) in body.match_indices("#[tauri::command]") {
        let after = &body[idx..];
        let name = after
            .lines()
            .find_map(|l| {
                l.trim()
                    .strip_prefix("pub fn ")
                    .or(l.trim().strip_prefix("pub async fn "))
            })
            .and_then(|l| l.split('(').next())
            .expect("a command declaration follows the attribute")
            .to_string();
        // The command body runs until the next command (or the end).
        let end = after[1..]
            .find("#[tauri::command]")
            .map(|o| o + 1)
            .unwrap_or(after.len());
        let fn_body = &after[..end];

        if READ_ONLY.contains(&name.as_str()) {
            assert!(
                !fn_body.contains("require_actions_enabled"),
                "{name} is listed read-only but gates on actions being enabled — \
                 update READ_ONLY or remove the gate"
            );
            continue;
        }
        assert!(
            fn_body.contains("require_actions_enabled"),
            "{name} reaches the write path but never calls require_actions_enabled; \
             a stale frontend must not be able to widen the blast radius"
        );
        checked += 1;
    }
    assert!(
        checked >= 4,
        "expected to find the gated commands, found {checked}"
    );
}

/// The invalidation rule lived in two places that disagreed: dispatch dropped the
/// device inventory only for `Reboot`, the poller dropped it for every settled
/// batch including scans. One function now answers for both.
#[test]
fn only_mutating_kinds_invalidate_and_a_scan_invalidates_nothing() {
    let state = AppState::new().expect("build state");

    // A scan changes nothing on the device, so neither cache is dropped.
    let before = state.settings_snapshot().actions.enabled;
    invalidate_after(ActionKind::OsPatchScan, false, &state);
    assert_eq!(state.settings_snapshot().actions.enabled, before);

    // Every mutating kind can move os.needsReboot, so both caches go.
    for kind in [
        ActionKind::OsPatchApply,
        ActionKind::SoftwarePatchApply,
        ActionKind::OsPatchRemediate,
        ActionKind::SoftwarePatchRemediate,
        ActionKind::Script,
        ActionKind::Reboot,
    ] {
        assert!(kind.is_mutating(), "{kind:?} must be mutating");
        assert!(
            kind.can_reboot(),
            "{kind:?} can set the pending-reboot flag, so the device cache must drop"
        );
        invalidate_after(kind, false, &state);
    }
    assert!(!ActionKind::OsPatchScan.can_reboot());
}

/// A dry run of a mutating kind changes nothing on the device, so it must not
/// drop the caches: `dry_run` is the default, and every default preview used to
/// cost a whole-fleet refetch on the next query.
#[test]
fn a_dry_run_invalidates_nothing() {
    let state = AppState::new().expect("build state");
    let (devices_before, current_before) = state.cache_epochs();
    invalidate_after(ActionKind::OsPatchRemediate, true, &state);
    assert_eq!(state.cache_epochs(), (devices_before, current_before));
    invalidate_after(ActionKind::OsPatchRemediate, false, &state);
    assert_ne!(state.cache_epochs(), (devices_before, current_before));
}

/// A remediation runs a library script chosen in Settings, so the job report and
/// the audit trail have to name it. It used to fall through to the bare kind
/// label, which said what was attempted but never which script did it.
#[test]
fn a_remediation_detail_names_the_script_it_ran() {
    let mut req = request(ActionKind::OsPatchRemediate, vec![1]);
    req.script_name = Some("Install-Approved-KBs".into());
    let detail = action_detail(&req);
    assert!(
        detail.contains("Install-Approved-KBs"),
        "the remediation script must be named: {detail}"
    );
    assert!(
        detail.contains(ActionKind::OsPatchRemediate.label()),
        "and the kind must still be there: {detail}"
    );

    // With no script name resolved it still degrades to the label rather than
    // inventing one.
    req.script_name = None;
    assert_eq!(action_detail(&req), ActionKind::OsPatchRemediate.label());
}
