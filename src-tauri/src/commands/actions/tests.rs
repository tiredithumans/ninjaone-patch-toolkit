use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::Utc;
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::confirm::{canonical_parameters, request_hash};
use super::dispatch::{action_detail, carries_dry_run_flag, invalidate_after, record_dispatch};
use super::plan::{
    composed_targets, dry_run_support, parameters_preview, per_device_parameters, resolve_run_as,
    summarize_names, untargeted_names,
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
        request: None,
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

fn library_script(id: i64, name: &str, variables: &[&str]) -> crate::model::AutomationScript {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": name,
        "scriptVariables": variables
            .iter()
            .map(|v| serde_json::json!({ "name": v }))
            .collect::<Vec<_>>(),
    }))
    .expect("script")
}

/// The dry-run gate reads the *resolved* script: a remediation kind's comes from
/// Settings, a built-in action has no `dryRun` at all, and a typed string is
/// refused before the library is even consulted — it is sent verbatim, so the
/// toolkit cannot add the flag to it.
#[test]
fn dry_run_support_follows_the_resolved_script() {
    use crate::actions::DryRunSupport;
    let library = vec![
        library_script(42, "Install-Kbs", &["kbAllowList", "dryRun"]),
        library_script(43, "Repair-WindowsUpdate", &[]),
    ];
    let lib = Ok(library.as_slice());
    let req = request(ActionKind::Script, vec![1]);

    assert_eq!(
        dry_run_support(&req, Some(&ScriptRef::Script { id: 42 }), lib),
        DryRunSupport::Declared
    );
    assert_eq!(
        dry_run_support(&req, Some(&ScriptRef::Script { id: 43 }), lib),
        DryRunSupport::NotDeclared {
            script: "Repair-WindowsUpdate".into()
        }
    );
    assert!(matches!(
        dry_run_support(&req, Some(&ScriptRef::Script { id: 99 }), lib),
        DryRunSupport::Unverified(why) if why.contains("#99")
    ));
    assert!(matches!(
        dry_run_support(&req, Some(&ScriptRef::Script { id: 42 }), Err("HTTP 503")),
        DryRunSupport::Unverified(why) if why.contains("HTTP 503")
    ));
    assert_eq!(
        dry_run_support(
            &req,
            Some(&ScriptRef::Action {
                uid: "built-in".into()
            }),
            lib
        ),
        DryRunSupport::BuiltInAction
    );

    // Even a script that declares dryRun is refused with a typed string.
    let typed = ActionRequest {
        parameters: Some("-Force".into()),
        ..req.clone()
    };
    assert_eq!(
        dry_run_support(&typed, Some(&ScriptRef::Script { id: 42 }), lib),
        DryRunSupport::TypedParameters
    );
    // A remediation kind has no typed string (it is ignored), so it goes by its
    // configured script.
    let remediation = ActionRequest {
        parameters: Some("-Force".into()),
        ..request(ActionKind::OsPatchRemediate, vec![1])
    };
    assert_eq!(
        dry_run_support(&remediation, Some(&ScriptRef::Script { id: 42 }), lib),
        DryRunSupport::Declared
    );
}

/// The dispatch-site half of the dry-run gate: a parameter string counts as a
/// preview only when it carries the exact token `build_parameters` composes.
#[test]
fn only_a_composed_dry_run_flag_counts_at_the_dispatch_site() {
    let composed = crate::actions::build_parameters(
        ActionKind::OsPatchRemediate,
        &["KB1".to_string()],
        RebootChoice::Never,
        true,
    );
    assert!(carries_dry_run_flag(&composed), "{composed}");
    let live = crate::actions::build_parameters(
        ActionKind::OsPatchRemediate,
        &["KB1".to_string()],
        RebootChoice::Never,
        false,
    );
    assert!(!carries_dry_run_flag(&live));
    assert!(!carries_dry_run_flag("-DryRun"));
    assert!(!carries_dry_run_flag("xdryRun=true"));
    assert!(!carries_dry_run_flag(""));
}

fn pending_job(id: u64, device_id: i64, kind: ActionKind, dispatched_ts: i64) -> JobReport {
    JobReport {
        id,
        batch_id: 1,
        device_id,
        device_name: format!("srv-{device_id}"),
        organization: "Contoso".into(),
        kind,
        detail: kind.label().into(),
        dry_run: false,
        state: JobState::Running,
        dispatched_at: String::new(),
        dispatched_ts,
        finished_at: None,
        duration_seconds: None,
        activity_id: None,
        series_uid: None,
        exit_code: None,
        request: None,
    }
}

fn mock_api(server: &MockServer) -> crate::api::NinjaApiClient {
    let http = reqwest::Client::new();
    let auth = crate::auth::AuthState::seeded(http.clone(), server.uri(), "test-token");
    crate::api::NinjaApiClient::new(http, auth)
}

/// The feed is per device, so a tick reads it once per device no matter how many
/// of that device's jobs are pending — and each job still resolves to the activity
/// its own kind emits. It used to cost one `/activities` request per *job*.
#[tokio::test]
async fn a_tick_reads_each_devices_feed_once_and_resolves_every_job_on_it() {
    let server = MockServer::start().await;
    let now = Utc::now();
    let ts = now.timestamp();
    Mock::given(method("GET"))
        .and(path("/api/v2/activities"))
        .and(query_param("df", "id=7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 902, "activityType": "SYSTEM", "activityTime": (ts - 1) as f64,
              "statusCode": "COMPLETED", "activityResult": "FAILURE" },
            { "id": 901, "activityType": "PATCH_MANAGEMENT", "activityTime": (ts - 2) as f64,
              "statusCode": "COMPLETED", "activityResult": "SUCCESS" },
        ])))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/v2/activities"))
        .and(query_param("df", "id=8"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([])))
        .expect(1)
        .mount(&server)
        .await;

    let pending = vec![
        pending_job(1, 7, ActionKind::OsPatchApply, ts - 30),
        pending_job(2, 7, ActionKind::Reboot, ts - 20),
        pending_job(3, 8, ActionKind::OsPatchScan, ts - 10),
    ];
    let mut claimed = HashSet::new();
    let mut confirmed = HashSet::new();
    let updates = poller::resolve_pending(
        &mock_api(&server),
        pending,
        &mut claimed,
        &mut confirmed,
        now,
        8,
    )
    .await;

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 2, "one read per device, not per job");

    let by_id = |id: u64| updates.iter().find(|j| j.id == id).expect("job");
    assert_eq!(by_id(1).state, JobState::Completed);
    assert_eq!(by_id(1).activity_id, Some(901));
    assert!(
        matches!(by_id(2).state, JobState::Failed(_)),
        "{:?}",
        by_id(2).state
    );
    assert_eq!(by_id(2).activity_id, Some(902));
    // An empty feed is lag, not failure.
    assert_eq!(by_id(3).state, JobState::Running);
    assert_eq!(claimed, HashSet::from([901, 902]));
}

/// Resolves `pending` against a device-7 feed holding one completed patch run —
/// the async half of a tick, ahead of `settle_tick`.
async fn resolved_against_a_completed_apply(pending: Vec<JobReport>) -> Vec<JobReport> {
    let server = MockServer::start().await;
    let now = Utc::now();
    Mock::given(method("GET"))
        .and(path("/api/v2/activities"))
        .and(query_param("df", "id=7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 901, "activityType": "PATCH_MANAGEMENT",
              "activityTime": (now.timestamp() - 2) as f64,
              "statusCode": "COMPLETED", "activityResult": "SUCCESS" },
        ])))
        .mount(&server)
        .await;
    let updates = poller::resolve_pending(
        &mock_api(&server),
        pending,
        &mut HashSet::new(),
        &mut HashSet::new(),
        now,
        8,
    )
    .await;
    assert_eq!(updates[0].state, JobState::Completed);
    updates
}

/// The control for the two tests below: a tick within one session applies,
/// invalidates, emits and audits as it always has.
#[tokio::test]
async fn a_tick_within_one_session_settles_its_jobs() {
    let state = AppState::new().expect("build state");
    let ts = Utc::now().timestamp();
    let apply = pending_job(1, 7, ActionKind::OsPatchApply, ts - 30);
    assert!(state.append_jobs(&state.job_session(), vec![apply]));
    let (session, pending) = state.pending_jobs();
    let updates = resolved_against_a_completed_apply(pending).await;

    let before = state.cache_epochs();
    let tick = poller::settle_tick(&state, &session, updates);

    assert_eq!(tick.applied.len(), 1);
    assert!(tick.settled_any);
    assert_ne!(state.cache_epochs(), before, "a settled apply invalidates");
    assert_eq!(tick.closing.len(), 1);
}

/// A tick awaits the feed reads, and a sign-out and sign-in (same instance) can
/// land in that time. `apply_job_updates` refused the departed session's rows, but
/// the tick still emitted them, and the frontend merged them into the next
/// operator's Jobs tab. The clear came first here, so it wrote the job's one close
/// (unresolved) and the tick writes none.
#[tokio::test]
async fn a_tick_that_spans_a_sign_out_emits_nothing_to_the_next_session() {
    let state = AppState::new().expect("build state");
    let ts = Utc::now().timestamp();
    let apply = pending_job(1, 7, ActionKind::OsPatchApply, ts - 30);
    assert!(state.append_jobs(&state.job_session(), vec![apply]));
    let (session, pending) = state.pending_jobs();
    let updates = resolved_against_a_completed_apply(pending).await;

    // Sign-out and sign-in while the tick was reading; the next operator dispatches.
    let closings = state.clear_jobs();
    assert_eq!(closings.len(), 1);
    assert_eq!(closings[0].outcome, audit::UNRESOLVED_SESSION_ENDED);
    let theirs = pending_job(2, 8, ActionKind::OsPatchApply, ts);
    assert!(state.append_jobs(&state.job_session(), vec![theirs]));

    let before = state.cache_epochs();
    let tick = poller::settle_tick(&state, &session, updates);

    assert!(tick.applied.is_empty(), "the old rows must not be emitted");
    assert!(!tick.settled_any);
    // Same instance: the device really changed and the next session's caches hold
    // that same fleet, so they are still dropped for it.
    assert_ne!(
        state.cache_epochs(),
        before,
        "a same-tenant apply still invalidates"
    );
    assert_eq!(state.jobs_snapshot()[0].state, JobState::Running);
    assert!(tick.closing.is_empty(), "clear_jobs already closed it");
}

/// The other order: the tick settles the job before the sign-out. The tick writes
/// the verdict close, the row is terminal, and `clear_jobs` writes nothing more.
#[tokio::test]
async fn a_tick_that_settles_before_the_sign_out_closes_the_job_once() {
    let state = AppState::new().expect("build state");
    let ts = Utc::now().timestamp();
    let apply = pending_job(1, 7, ActionKind::OsPatchApply, ts - 30);
    assert!(state.append_jobs(&state.job_session(), vec![apply]));
    let (session, pending) = state.pending_jobs();
    let updates = resolved_against_a_completed_apply(pending).await;

    let tick = poller::settle_tick(&state, &session, updates);
    assert_eq!(tick.closing.len(), 1);
    assert_eq!(tick.closing[0].instance, session.instance());

    assert!(
        state.clear_jobs().is_empty(),
        "a settled row is not closed again"
    );
}

/// Across a tenant switch the feed was read through the *new* instance's client,
/// so a device id there is another machine and its verdict means nothing for this
/// job. The switch's `clear_jobs` already closed it as unresolved, labelled with the
/// instance it was sent to; the tick adds no second line, and no verdict read from
/// the wrong tenant.
#[tokio::test]
async fn a_tick_that_spans_an_instance_switch_writes_no_verdict() {
    let state = AppState::new().expect("build state");
    let ts = Utc::now().timestamp();
    let apply = pending_job(1, 7, ActionKind::OsPatchApply, ts - 30);
    assert!(state.append_jobs(&state.job_session(), vec![apply]));
    let (session, pending) = state.pending_jobs();
    let updates = resolved_against_a_completed_apply(pending).await;

    state.seed_settings(crate::settings::Settings {
        instance_base_url: "https://other.ninjarmm.com".into(),
        ..state.settings_snapshot()
    });
    let closings = state.clear_jobs();
    assert_eq!(closings.len(), 1, "the switch closes the row once");
    assert_eq!(closings[0].outcome, audit::UNRESOLVED_SESSION_ENDED);
    assert_eq!(closings[0].instance, session.instance());

    let before = state.cache_epochs();
    let tick = poller::settle_tick(&state, &session, updates);

    assert!(tick.applied.is_empty());
    assert_eq!(state.cache_epochs(), before);
    assert!(tick.closing.is_empty(), "no second record for the same job");
}

/// Two jobs of the *same* kind on one device share one read, and the claimed-id
/// exclusion still hands each its own activity rather than both the newest.
#[tokio::test]
async fn two_same_kind_jobs_on_one_device_share_a_read_but_not_an_activity() {
    let server = MockServer::start().await;
    let now = Utc::now();
    let ts = now.timestamp();
    Mock::given(method("GET"))
        .and(path("/api/v2/activities"))
        .and(query_param("df", "id=7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 12, "activityType": "PATCH_MANAGEMENT", "activityTime": (ts - 1) as f64,
              "statusCode": "COMPLETED", "activityResult": "FAILURE" },
            { "id": 11, "activityType": "PATCH_MANAGEMENT", "activityTime": (ts - 5) as f64,
              "statusCode": "COMPLETED", "activityResult": "SUCCESS" },
        ])))
        .expect(1)
        .mount(&server)
        .await;

    let pending = vec![
        pending_job(1, 7, ActionKind::OsPatchScan, ts - 30),
        pending_job(2, 7, ActionKind::OsPatchApply, ts - 20),
    ];
    let updates = poller::resolve_pending(
        &mock_api(&server),
        pending,
        &mut HashSet::new(),
        &mut HashSet::new(),
        now,
        8,
    )
    .await;

    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
    assert_ne!(
        updates[0].activity_id, updates[1].activity_id,
        "two jobs must never bind the same activity"
    );
    assert!(updates.iter().all(|j| j.state.is_terminal()));
}

/// The shared read is floored at the device's *earliest* dispatch, so no job on
/// it loses an activity it could have matched, and it narrows to `seriesUid` only
/// once that uid has been seen on an activity — a dispatch response's bare `uid`
/// may be one no activity carries, and a read narrowed to it would starve the job.
#[test]
fn feed_reads_group_by_device_and_narrow_only_to_a_confirmed_series() {
    let mut a = pending_job(1, 7, ActionKind::Script, 1_000);
    a.series_uid = Some("uid-a".into());
    let b = pending_job(2, 7, ActionKind::Reboot, 900);
    let mut c = pending_job(3, 8, ActionKind::Script, 2_000);
    c.series_uid = Some("uid-c".into());

    let unconfirmed = poller::feed_reads(&[a.clone(), b.clone(), c.clone()], &HashSet::new());
    assert_eq!(
        unconfirmed,
        vec![
            poller::FeedRead {
                device_id: 7,
                series_uid: None,
                since_ts: 895,
            },
            poller::FeedRead {
                device_id: 8,
                series_uid: None,
                since_ts: 1_995,
            },
        ]
    );

    let confirmed = HashSet::from(["uid-a".to_string(), "uid-c".to_string()]);
    let reads = poller::feed_reads(&[a, b, c], &confirmed);
    // Device 7 has two pending jobs, so it stays device-wide.
    assert_eq!(reads[0].series_uid, None);
    assert_eq!(reads[1].series_uid.as_deref(), Some("uid-c"));
}

/// A series uid is confirmed by the device-wide feed and then used on the next
/// tick — with the device `df` still sent, so it only ever narrows.
#[tokio::test]
async fn a_confirmed_series_narrows_the_next_read() {
    let server = MockServer::start().await;
    let now = Utc::now();
    let ts = now.timestamp();
    Mock::given(method("GET"))
        .and(path("/api/v2/activities"))
        .and(query_param("df", "id=7"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!([
            { "id": 950, "activityType": "SCRIPTING", "activityTime": ts as f64,
              "seriesUid": "uid-9", "statusCode": "IN_PROCESS" },
        ])))
        .mount(&server)
        .await;

    let mut job = pending_job(1, 7, ActionKind::Script, ts - 10);
    job.series_uid = Some("uid-9".into());
    let api = mock_api(&server);
    let mut claimed = HashSet::new();
    let mut confirmed = HashSet::new();

    let first =
        poller::resolve_pending(&api, vec![job], &mut claimed, &mut confirmed, now, 8).await;
    assert_eq!(first[0].state, JobState::Running);
    assert!(confirmed.contains("uid-9"));
    poller::resolve_pending(&api, first, &mut claimed, &mut confirmed, now, 8).await;

    let requests = server.received_requests().await.expect("requests");
    let urls: Vec<String> = requests.iter().map(|r| r.url.to_string()).collect();
    assert!(!urls[0].contains("seriesUid"), "{}", urls[0]);
    assert!(urls[1].contains("seriesUid=uid-9"), "{}", urls[1]);
    assert!(urls[1].contains("df=id%3D7"), "{}", urls[1]);
}

/// Records when each `/activities` read arrives; every answer takes `delay`, so a
/// read is in flight from its arrival until `delay` later.
struct ArrivalLog {
    arrivals: std::sync::Arc<std::sync::Mutex<Vec<std::time::Instant>>>,
    delay: std::time::Duration,
}

impl wiremock::Respond for ArrivalLog {
    fn respond(&self, _: &wiremock::Request) -> ResponseTemplate {
        self.arrivals
            .lock()
            .expect("arrivals")
            .push(std::time::Instant::now());
        ResponseTemplate::new(200)
            .set_body_json(json!([]))
            .set_delay(self.delay)
    }
}

/// A tick's reads are bounded like the dispatch POSTs. They were spawned all at
/// once, so a 500-device batch put 500 GETs on the wire every 15 s, and a 429
/// parked every one of them on its `Retry-After` together.
#[tokio::test]
async fn a_tick_keeps_at_most_the_cap_of_reads_in_flight() {
    const CAP: usize = 2;
    const DEVICES: i64 = 6;
    let delay = std::time::Duration::from_millis(300);
    let arrivals = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v2/activities"))
        .respond_with(ArrivalLog {
            arrivals: std::sync::Arc::clone(&arrivals),
            delay,
        })
        .mount(&server)
        .await;

    let now = Utc::now();
    let pending: Vec<JobReport> = (0..DEVICES)
        .map(|d| {
            pending_job(
                d as u64 + 1,
                d + 1,
                ActionKind::OsPatchScan,
                now.timestamp(),
            )
        })
        .collect();
    let updates = poller::resolve_pending(
        &mock_api(&server),
        pending,
        &mut HashSet::new(),
        &mut HashSet::new(),
        now,
        CAP,
    )
    .await;
    assert_eq!(updates.len(), DEVICES as usize);

    let arrivals = arrivals.lock().expect("arrivals").clone();
    assert_eq!(
        arrivals.len(),
        DEVICES as usize,
        "still one read per device"
    );
    // Reads that arrived within (most of) one response delay of each other were
    // in flight together.
    let overlap = delay - std::time::Duration::from_millis(100);
    let peak = arrivals
        .iter()
        .map(|start| {
            arrivals
                .iter()
                .filter(|t| **t >= *start && t.duration_since(*start) < overlap)
                .count()
        })
        .max()
        .unwrap_or(0);
    assert!(peak <= CAP, "{peak} reads were in flight at once");
}

/// A retry is rebuilt from what the job recorded, so the job must record this
/// device's own targets (never the batch union), the *resolved* run-as, and
/// everything else the re-plan reads — except the maintenance-window override,
/// which is re-decided at retry time.
#[test]
fn a_job_records_what_a_retry_needs_for_its_own_device() {
    let mut req = request(ActionKind::OsPatchRemediate, vec![1, 2]);
    req.device_targets = HashMap::from([
        (1, vec!["KB500".to_string()]),
        (2, vec!["KB600".to_string(), "KB601".to_string()]),
    ]);
    req.reboot = RebootChoice::Auto;
    req.include_offline = true;
    req.override_window = true;
    req.dry_run = true;
    req.parameters = Some("ignored for a remediation".into());

    let one = job_request(&req, Some("system"), 1);
    assert_eq!(one.targets, vec!["KB500".to_string()]);
    assert_eq!(one.run_as.as_deref(), Some("system"));
    assert_eq!(one.reboot, RebootChoice::Auto);
    assert!(one.include_offline);
    assert_eq!(
        one.parameters, None,
        "only a Script's typed string is carried"
    );
    assert_eq!(job_request(&req, Some("system"), 2).targets.len(), 2);

    let mut script = request(ActionKind::Script, vec![1]);
    script.script_id = Some(42);
    script.parameters = Some("  -Verbose ".into());
    let rec = job_request(&script, Some("local-admin"), 1);
    assert_eq!(rec.script_id, Some(42));
    assert_eq!(rec.parameters.as_deref(), Some("-Verbose"));

    let mut reboot = request(ActionKind::Reboot, vec![1]);
    reboot.reboot_mode = Some(RebootMode::Forced);
    reboot.reason = Some("July cycle".into());
    let rec = job_request(&reboot, None, 1);
    assert_eq!(rec.reboot_mode, Some(RebootMode::Forced));
    assert_eq!(rec.reason.as_deref(), Some("July cycle"));
    assert_eq!(rec.run_as, None, "the native endpoints run as the agent");
}

fn scan_context(
    api: crate::api::NinjaApiClient,
    live: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> dispatch::DispatchContext {
    let still_current: crate::api::SendGuard =
        std::sync::Arc::new(move || live.load(std::sync::atomic::Ordering::SeqCst));
    dispatch::DispatchContext {
        api: api.with_send_guard(std::sync::Arc::clone(&still_current)),
        kind: ActionKind::OsPatchScan,
        script: None,
        run_as: String::new(),
        parameters: BTreeMap::new(),
        reason: String::new(),
        reboot_mode: RebootMode::Normal,
        dry_run: false,
        window_overridden: false,
        detail: "scan".into(),
        instance: "https://a.example".into(),
        client_id: None,
        confirm_prefix: None,
        batch_id: 1,
        id_base: 1,
        job_requests: BTreeMap::new(),
        still_current,
    }
}

/// A batch wider than the semaphore queues devices behind the ones being sent. A
/// sign-out or tenant switch while they waited used to let every one of them POST
/// anyway, under a session that never confirmed the action. The session is checked
/// after the permit — the long wait — so a device queued across the change is not
/// sent at all.
#[tokio::test]
async fn a_device_queued_when_the_session_ends_is_not_sent() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::Poll;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/7/patch/os/scan"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let live = Arc::new(AtomicBool::new(true));
    let ctx = scan_context(mock_api(&server), Arc::clone(&live));
    let sem = tokio::sync::Semaphore::new(1);

    // Another device holds the only permit; ours queues behind it.
    let ahead = sem.acquire().await.expect("permit");
    let queued = dispatch::send_if_current(&ctx, 7, &sem);
    tokio::pin!(queued);
    let waiting =
        std::future::poll_fn(|cx| Poll::Ready(queued.as_mut().poll(cx).is_pending())).await;
    assert!(waiting, "still waiting for a permit");

    // The session ends while it waits.
    live.store(false, Ordering::SeqCst);
    drop(ahead);

    assert!(
        queued.await.outcome.is_none(),
        "a queued device must not be sent"
    );
    assert!(
        server
            .received_requests()
            .await
            .expect("requests")
            .is_empty()
    );

    // A live session sends as before.
    live.store(true, Ordering::SeqCst);
    let sent = dispatch::send_if_current(&ctx, 7, &sem).await.outcome;
    assert!(matches!(sent, Some(Ok(None))), "{sent:?}");
    assert_eq!(server.received_requests().await.expect("requests").len(), 1);
}

/// A device's dispatch time is when its POST goes out, not when its batch began.
/// It was stamped before the permit, so a device queued behind slow sends carried
/// a dispatch time minutes before its request: the wait came off its job timeout
/// and lowered the poller's activity floor below the send.
#[tokio::test]
async fn a_queued_device_is_stamped_when_its_turn_comes() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/7/patch/os/scan"))
        .respond_with(ResponseTemplate::new(204).set_delay(std::time::Duration::from_millis(400)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/8/patch/os/scan"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let ctx = scan_context(mock_api(&server), Arc::new(AtomicBool::new(true)));
    let sem = tokio::sync::Semaphore::new(1);

    // Device 7 is polled first and takes the only permit; device 8 queues.
    let first = async {
        let turn = dispatch::send_if_current(&ctx, 7, &sem).await;
        (turn, Utc::now())
    };
    let ((first, first_done), second) =
        tokio::join!(first, dispatch::send_if_current(&ctx, 8, &sem));

    assert!(matches!(first.outcome, Some(Ok(None))));
    assert!(matches!(second.outcome, Some(Ok(None))));
    assert!(
        second.at >= first_done - chrono::Duration::milliseconds(50),
        "device 8 was stamped at {} but could only send after {}",
        second.at,
        first_done
    );
}

/// The same, one level up: the job a queued device records carries the time its
/// turn came, not the time its task started. The delay is over a second because
/// `dispatched_ts` has one-second resolution.
#[tokio::test]
async fn a_queued_devices_job_records_when_it_was_sent() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/7/patch/os/scan"))
        .respond_with(ResponseTemplate::new(204).set_delay(std::time::Duration::from_millis(1100)))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/8/patch/os/scan"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let ctx = scan_context(mock_api(&server), Arc::new(AtomicBool::new(true)));
    let sem = tokio::sync::Semaphore::new(1);
    let target = |device_id: i64| PlannedTarget {
        device_id,
        device_name: format!("srv-{device_id}"),
        organization: "Contoso".into(),
        offline: false,
    };
    let (seven, eight) = (target(7), target(8));

    // Device 7 is polled first and takes the only permit; device 8 queues.
    let first = async {
        let job = dispatch::send_and_record(&ctx, &seven, 1, &sem).await;
        (job, Utc::now())
    };
    let ((first, first_done), second) =
        tokio::join!(first, dispatch::send_and_record(&ctx, &eight, 2, &sem));

    assert_eq!(first.state, JobState::Running);
    assert_eq!(second.state, JobState::Running);
    assert!(
        second.dispatched_ts >= first_done.timestamp(),
        "device 8's job says it was sent at {} ({}), but it could only send after {}",
        second.dispatched_ts,
        second.dispatched_at,
        first_done
    );
}

/// The session check before the send is not enough on its own: a 429 parks the
/// POST for its `Retry-After`, and every retry reads the token live. A sign-out
/// and another operator's sign-in in that wait re-sent the departed session's
/// action under the new operator's grant. The retry must ask the session again,
/// and a request stopped there was rejected every time it went out — not sent.
#[tokio::test]
async fn a_dispatch_retry_after_the_session_ends_is_not_sent() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/7/patch/os/scan"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "1"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v2/device/7/patch/os/scan"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&server)
        .await;

    let live = Arc::new(AtomicBool::new(true));
    let ctx = scan_context(mock_api(&server), Arc::clone(&live));
    let sem = tokio::sync::Semaphore::new(1);

    // The session ends while the first attempt's 429 backoff is running.
    let ender = {
        let live = Arc::clone(&live);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            live.store(false, Ordering::SeqCst);
        })
    };
    let outcome = dispatch::send_if_current(&ctx, 7, &sem)
        .await
        .outcome
        .expect("the session was live when the permit came");
    ender.await.expect("ender");

    let mut job = pending_job(1, 7, ActionKind::OsPatchScan, Utc::now().timestamp());
    record_dispatch(&mut job, outcome, Utc::now());
    assert!(
        matches!(&job.state, JobState::Skipped(why) if why.starts_with("not sent")),
        "{:?}",
        job.state
    );
    server.verify().await;
}

/// A device's progress event carries its row, and the frontend merges it into the
/// Jobs list. Once the session has ended that list belongs to the next one, so an
/// outcome landing after the sign-out must not be emitted at all.
#[tokio::test]
async fn a_dispatch_outcome_after_the_session_ends_is_not_emitted() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let server = MockServer::start().await;
    let live = Arc::new(AtomicBool::new(true));
    let ctx = scan_context(mock_api(&server), Arc::clone(&live));
    let job = pending_job(1, 7, ActionKind::OsPatchScan, 0);

    let ev = dispatch::device_progress(&ctx, 1, 2, &job).expect("live session emits");
    assert_eq!(ev.jobs.len(), 1);

    live.store(false, Ordering::SeqCst);
    assert!(dispatch::device_progress(&ctx, 2, 2, &job).is_none());
}
