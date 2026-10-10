use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Local, TimeZone, Utc};
use serde::Deserialize;
use serde_json::json;

use super::planning::window_is_open;
use super::*;
use crate::model::{Activity, Device, Patch};
use crate::settings::ActionSettings;

fn device(id: i64, name: &str, org: i64, offline: bool) -> Device {
    serde_json::from_value(json!({
        "id": id,
        "systemName": name,
        "organizationId": org,
        "offline": offline,
    }))
    .expect("device")
}

fn orgs() -> HashMap<i64, String> {
    HashMap::from([(1, "Contoso".to_string()), (2, "Fabrikam".to_string())])
}

/// A Wednesday at 03:00 local — inside the default Mon–Fri 02:00–05:00 window.
fn inside_window() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 7, 29, 3, 0, 0).unwrap()
}

/// Same Wednesday at 13:00 local — outside it.
fn outside_window() -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 7, 29, 13, 0, 0).unwrap()
}

fn input<'a>(
    kind: ActionKind,
    ids: &'a [i64],
    devices: &'a [Device],
    names: &'a HashMap<i64, String>,
    settings: &'a ActionSettings,
) -> PlanInput<'a> {
    PlanInput {
        kind,
        device_ids: ids,
        devices,
        org_names: names,
        settings,
        include_offline: false,
        override_window: false,
        reboot_mode: None,
        reboot: RebootChoice::Never,
        dry_run: false,
        // Enough that the remediation kinds aren't blocked for an empty
        // selection; the tests that care about that set it explicitly.
        targets: &["KB5040434"],
        // Honest by default so the dry-run tests that are about something else
        // aren't blocked by the script check; the tests that care set it.
        dry_run_support: DryRunSupport::Declared,
        current_patches: None,
        now: inside_window(),
    }
}

/// Settings with both remediation scripts configured, so the remediation kinds
/// are available.
fn with_scripts() -> ActionSettings {
    ActionSettings {
        os_patch_script_id: Some(42),
        software_patch_script_id: Some(43),
        ..ActionSettings::default()
    }
}

#[test]
fn plan_skips_offline_devices_unless_opted_in() {
    let devices = vec![device(1, "srv-a", 1, false), device(2, "srv-b", 1, true)];
    let names = orgs();
    let ids = [1, 2];
    let settings = ActionSettings::default();

    let p = plan(input(
        ActionKind::OsPatchApply,
        &ids,
        &devices,
        &names,
        &settings,
    ));
    assert_eq!(p.eligible.len(), 1);
    assert_eq!(p.skipped.len(), 1);
    assert!(p.skipped[0].reason.contains("offline"));

    // Opting in requires BOTH the per-request flag and the setting.
    let opted = ActionSettings {
        allow_offline_targets: true,
        ..ActionSettings::default()
    };
    let p = plan(PlanInput {
        include_offline: true,
        ..input(ActionKind::OsPatchApply, &ids, &devices, &names, &opted)
    });
    assert_eq!(p.eligible.len(), 2);
    assert!(
        p.warnings.iter().any(|w| w.contains("queue")),
        "including an offline device must still warn that it gets queued"
    );
}

#[test]
fn plan_blocks_over_the_device_cap() {
    let devices: Vec<Device> = (1..=30)
        .map(|i| device(i, &format!("srv{i}"), 1, false))
        .collect();
    let ids: Vec<i64> = (1..=30).collect();
    let names = orgs();
    let settings = ActionSettings::default(); // cap 25

    let p = plan(input(
        ActionKind::OsPatchApply,
        &ids,
        &devices,
        &names,
        &settings,
    ));
    assert!(p.is_blocked());
    assert!(p.blockers[0].contains("25-device limit"));

    // A scan doesn't change the device, so the cap doesn't apply.
    let p = plan(input(
        ActionKind::OsPatchScan,
        &ids,
        &devices,
        &names,
        &settings,
    ));
    assert!(
        !p.is_blocked(),
        "scans are exempt from the blast-radius cap"
    );
}

#[test]
fn plan_blocks_cross_org_beyond_the_org_cap() {
    let devices = vec![device(1, "srv-a", 1, false), device(2, "srv-b", 2, false)];
    let ids = [1, 2];
    let names = orgs();
    let settings = ActionSettings::default(); // 1 org

    let p = plan(input(ActionKind::Reboot, &ids, &devices, &names, &settings));
    assert!(p.is_blocked());
    assert!(
        p.blockers.iter().any(|b| b.contains("2 organizations")),
        "a cross-tenant dispatch must be a blocker: {:?}",
        p.blockers
    );

    let wider = ActionSettings {
        max_orgs_per_action: 2,
        ..ActionSettings::default()
    };
    assert!(!plan(input(ActionKind::Reboot, &ids, &devices, &names, &wider)).is_blocked());
}

#[test]
fn plan_blocks_outside_the_maintenance_window_and_honors_the_override_flag() {
    let devices = vec![device(1, "srv-a", 1, false)];
    let ids = [1];
    let names = orgs();
    let gated = ActionSettings {
        require_maintenance_window: true,
        ..ActionSettings::default()
    };

    // Inside the window: fine.
    assert!(
        !plan(input(
            ActionKind::OsPatchApply,
            &ids,
            &devices,
            &names,
            &gated
        ))
        .is_blocked()
    );

    // Outside it: blocked.
    let p = plan(PlanInput {
        now: outside_window(),
        ..input(ActionKind::OsPatchApply, &ids, &devices, &names, &gated)
    });
    assert!(p.is_blocked());
    assert!(p.blockers[0].contains("maintenance window"));

    // Asking to override without the setting enabled changes nothing.
    let p = plan(PlanInput {
        now: outside_window(),
        override_window: true,
        ..input(ActionKind::OsPatchApply, &ids, &devices, &names, &gated)
    });
    assert!(
        p.is_blocked(),
        "the override must be enabled in Settings too"
    );

    let overridable = ActionSettings {
        allow_window_override: true,
        ..gated
    };
    let p = plan(PlanInput {
        now: outside_window(),
        override_window: true,
        ..input(
            ActionKind::OsPatchApply,
            &ids,
            &devices,
            &names,
            &overridable,
        )
    });
    assert!(!p.is_blocked());
    assert!(p.warnings.iter().any(|w| w.contains("override")));
}

/// The blocker used to say "enable the override in Settings", which named only
/// the permission: the per-dispatch request had no control at all. It must name
/// both steps that apply, and the window it is enforcing.
#[test]
fn the_window_blocker_names_the_real_steps_and_the_window() {
    let devices = vec![device(1, "srv-a", 1, false)];
    let ids = [1];
    let names = orgs();
    let gated = ActionSettings {
        require_maintenance_window: true,
        ..ActionSettings::default()
    };

    // Override not permitted: both the Settings permission and the per-dispatch
    // checkbox are named, as is the window and the clock it is read on.
    let p = plan(PlanInput {
        now: outside_window(),
        ..input(ActionKind::OsPatchApply, &ids, &devices, &names, &gated)
    });
    let b = p
        .blockers
        .iter()
        .find(|b| b.contains("maintenance window"))
        .expect("a window blocker");
    assert!(b.contains("Mon/Tue/Wed/Thu/Fri 02:00–05:00"), "{b}");
    assert!(b.contains("this computer's time (UTC"), "{b}");
    assert!(b.contains("Allow overriding the maintenance window"), "{b}");
    assert!(
        b.contains("Override the maintenance window for this dispatch"),
        "{b}"
    );
    assert!(!p.window_overridden);

    // Permitted but not requested: only the per-dispatch step is left to do.
    let overridable = ActionSettings {
        allow_window_override: true,
        ..gated.clone()
    };
    let p = plan(PlanInput {
        now: outside_window(),
        ..input(
            ActionKind::OsPatchApply,
            &ids,
            &devices,
            &names,
            &overridable,
        )
    });
    let b = p
        .blockers
        .iter()
        .find(|b| b.contains("maintenance window"))
        .expect("a window blocker");
    assert!(
        b.contains("Override the maintenance window for this dispatch"),
        "{b}"
    );
    assert!(
        !b.contains("Settings"),
        "the Settings step is already done: {b}"
    );

    // Used: the plan says so, which is what the audit trail records.
    let p = plan(PlanInput {
        now: outside_window(),
        override_window: true,
        ..input(
            ActionKind::OsPatchApply,
            &ids,
            &devices,
            &names,
            &overridable,
        )
    });
    assert!(!p.is_blocked(), "{:?}", p.blockers);
    assert!(p.window_overridden);
    assert!(p.warnings.iter().any(|w| w.contains("audit trail")));

    // Requested inside an open window: nothing was bypassed, so nothing is
    // recorded as an override.
    let p = plan(PlanInput {
        override_window: true,
        ..input(
            ActionKind::OsPatchApply,
            &ids,
            &devices,
            &names,
            &overridable,
        )
    });
    assert!(!p.window_overridden);

    // A scan changes nothing, so the window never applies to it.
    let p = plan(PlanInput {
        now: outside_window(),
        ..input(ActionKind::OsPatchScan, &ids, &devices, &names, &gated)
    });
    assert!(!p.is_blocked());
}

/// A dry run only appends `dryRun=true`; a script that doesn't read it runs for
/// real under a "Dry run" label. Every way the planner can fail to *know* the
/// script reads it is a blocker — only a declared `dryRun` passes.
#[test]
fn a_dry_run_is_allowed_only_for_a_script_that_declares_dry_run() {
    let devices = vec![device(1, "srv-a", 1, false)];
    let ids = [1];
    let names = orgs();
    let settings = with_scripts();

    for kind in [ActionKind::Script, ActionKind::OsPatchRemediate] {
        let p = plan(PlanInput {
            dry_run: true,
            dry_run_support: DryRunSupport::Declared,
            ..input(kind, &ids, &devices, &names, &settings)
        });
        assert!(!p.is_blocked(), "{kind:?}: {:?}", p.blockers);

        for (support, says) in [
            (
                DryRunSupport::NotDeclared {
                    script: "Install-Kbs".into(),
                },
                "\"Install-Kbs\" declares no dryRun",
            ),
            (DryRunSupport::BuiltInAction, "built-in actions"),
            (DryRunSupport::TypedParameters, "Hand-typed parameters"),
            (
                DryRunSupport::Unverified("library unreachable".into()),
                "library unreachable",
            ),
            (DryRunSupport::NotChecked, "Couldn't confirm"),
        ] {
            let p = plan(PlanInput {
                dry_run: true,
                dry_run_support: support.clone(),
                ..input(kind, &ids, &devices, &names, &settings)
            });
            assert!(
                p.blockers
                    .iter()
                    .any(|b| b.contains(says) && b.contains("for real")),
                "{kind:?} with {support:?}: {:?}",
                p.blockers
            );
        }

        // Not a dry run: whether the script could preview is irrelevant.
        let p = plan(PlanInput {
            dry_run: false,
            dry_run_support: DryRunSupport::NotDeclared {
                script: "Install-Kbs".into(),
            },
            ..input(kind, &ids, &devices, &names, &settings)
        });
        assert!(!p.is_blocked(), "{kind:?}: {:?}", p.blockers);
    }
}

fn cached(patches: &[Patch]) -> CachedFamily<'_> {
    CachedFamily {
        patches,
        fetched_at: Utc.with_ymd_and_hms(2026, 7, 29, 10, 0, 0).unwrap(),
    }
}

fn os_patch(device_id: i64, kb: &str, status: &str) -> Patch {
    serde_json::from_value(json!({
        "id": format!("{device_id}-{kb}"),
        "name": format!("Update {kb}"),
        "kbNumber": kb,
        "severity": "CRITICAL",
        "status": status,
        "type": "PATCH",
        "deviceId": device_id,
        "timestamp": 1_750_000_000.0,
    }))
    .expect("os patch")
}

fn planned(id: i64, name: &str) -> PlannedTarget {
    PlannedTarget {
        device_id: id,
        device_name: name.into(),
        organization: "Contoso".into(),
        offline: false,
    }
}

/// "Apply all" installs what NinjaOne has APPROVED — not what the operator
/// ticked, and not what is pending approval (`MANUAL`). The preview counts
/// exactly that, per device, and only for the devices in the plan.
#[test]
fn apply_preview_counts_approved_and_pending_manual_per_device() {
    let patches = vec![
        os_patch(1, "KB1", "APPROVED"),
        os_patch(1, "KB2", "approved"), // case-insensitive
        os_patch(1, "KB3", "MANUAL"),
        os_patch(1, "KB4", "REJECTED"),
        os_patch(2, "KB1", "MANUAL"),
        os_patch(3, "KB1", "APPROVED"), // not in the plan
    ];
    let eligible = vec![planned(1, "srv-a"), planned(2, "srv-b")];

    let p = apply_preview(ActionKind::OsPatchApply, &eligible, Some(cached(&patches)))
        .expect("a native apply gets a preview");
    assert!(p.known);
    assert_eq!(p.family, "OS");
    assert_eq!(
        p.devices,
        vec![
            ApplyPreviewDevice {
                device_id: 1,
                device_name: "srv-a".into(),
                approved: 2,
                pending_manual: 1,
            },
            ApplyPreviewDevice {
                device_id: 2,
                device_name: "srv-b".into(),
                approved: 0,
                pending_manual: 1,
            },
        ]
    );
    assert_eq!((p.approved_total, p.pending_manual_total), (2, 2));
    assert_eq!(
        p.data_fetched_at.as_deref(),
        Some("2026-07-29 10:00:00 UTC")
    );

    let sw = apply_preview(ActionKind::SoftwarePatchApply, &eligible, None).unwrap();
    assert_eq!(sw.family, "software");
    assert!(!sw.known, "a cold cache is unknown, not zero");
    assert!(sw.devices.is_empty() && sw.data_fetched_at.is_none());

    for kind in [
        ActionKind::OsPatchRemediate,
        ActionKind::OsPatchScan,
        ActionKind::Reboot,
        ActionKind::Script,
    ] {
        assert!(
            apply_preview(kind, &eligible, Some(cached(&patches))).is_none(),
            "{kind:?} installs no approved backlog"
        );
    }
}

#[test]
fn apply_all_warns_for_a_device_with_nothing_approved_but_only_when_known() {
    let devices = vec![device(1, "srv-a", 1, false), device(2, "srv-b", 1, false)];
    let ids = [1, 2];
    let names = orgs();
    let settings = ActionSettings::default();
    let patches = vec![os_patch(1, "KB1", "APPROVED"), os_patch(2, "KB1", "MANUAL")];

    let p = plan(PlanInput {
        current_patches: Some(cached(&patches)),
        ..input(ActionKind::OsPatchApply, &ids, &devices, &names, &settings)
    });
    assert!(!p.is_blocked(), "a warning, not a blocker");
    let w = p
        .warnings
        .iter()
        .find(|w| w.contains("nothing will install"))
        .expect("a zero-approved warning");
    assert!(w.contains("1 device(s)") && w.contains("srv-b"), "{w}");
    assert!(!w.contains("srv-a"), "{w}");
    assert!(
        w.contains("MANUAL") && w.contains("approved in NinjaOne"),
        "{w}"
    );
    assert_eq!(p.apply_preview.as_ref().map(|a| a.approved_total), Some(1));

    // Cold cache: the preview says unknown and the planner does not guess.
    let p = plan(input(
        ActionKind::OsPatchApply,
        &ids,
        &devices,
        &names,
        &settings,
    ));
    assert!(p.apply_preview.as_ref().is_some_and(|a| !a.known));
    assert!(
        !p.warnings
            .iter()
            .any(|w| w.contains("nothing will install"))
    );
}

#[test]
fn a_wrapping_window_spans_midnight() {
    let s = ActionSettings {
        require_maintenance_window: true,
        window_days: vec![3], // Wednesday
        window_start_minute: 22 * 60,
        window_end_minute: 4 * 60,
        ..ActionSettings::default()
    };
    // Wednesday 23:00 — after the Wednesday open.
    assert!(window_is_open(
        &s,
        Local.with_ymd_and_hms(2026, 7, 29, 23, 0, 0).unwrap()
    ));
    // Thursday 02:00 — still inside the window that opened Wednesday.
    assert!(window_is_open(
        &s,
        Local.with_ymd_and_hms(2026, 7, 30, 2, 0, 0).unwrap()
    ));
    // Thursday 05:00 — closed.
    assert!(!window_is_open(
        &s,
        Local.with_ymd_and_hms(2026, 7, 30, 5, 0, 0).unwrap()
    ));
    // Wednesday 12:00 — before it opens.
    assert!(!window_is_open(
        &s,
        Local.with_ymd_and_hms(2026, 7, 29, 12, 0, 0).unwrap()
    ));
}

/// The one thing an operator cannot deduce from the UI: ticking a single row
/// under "By patch" grouping and pressing Apply installs the device's whole
/// approved backlog, because the endpoint has no per-patch variant.
#[test]
fn apply_warns_that_it_is_not_limited_to_the_selected_patches() {
    let devices = vec![device(1, "srv-a", 1, false)];
    let ids = [1];
    let names = orgs();
    let settings = ActionSettings::default();

    for (kind, what) in [
        (ActionKind::OsPatchApply, "OS"),
        (ActionKind::SoftwarePatchApply, "software"),
    ] {
        let p = plan(input(kind, &ids, &devices, &names, &settings));
        let w = p
            .warnings
            .iter()
            .find(|w| w.contains("every approved"))
            .unwrap_or_else(|| panic!("{kind:?} must warn that it is not per-patch"));
        assert!(w.contains(what), "{w}");
        assert!(
            w.contains("configure a remediation script"),
            "with no script configured, point at the setting that enables the other path: {w}"
        );
    }

    // Once a remediation script exists, the warning names the action that would
    // honor the selection rather than telling the operator to go configure one.
    let p = plan(input(
        ActionKind::OsPatchApply,
        &ids,
        &devices,
        &names,
        &with_scripts(),
    ));
    let w = p
        .warnings
        .iter()
        .find(|w| w.contains("every approved"))
        .expect("warning");
    assert!(w.contains(ActionKind::OsPatchRemediate.label()), "{w}");
    assert!(!w.contains("configure a remediation script"), "{w}");

    // A scan installs nothing, so the warning would be noise.
    let p = plan(input(
        ActionKind::OsPatchScan,
        &ids,
        &devices,
        &names,
        &settings,
    ));
    assert!(!p.warnings.iter().any(|w| w.contains("every approved")));
}

/// The targeted half of each pair fails closed in the two ways that would
/// otherwise produce a NinjaOne activity indistinguishable from a successful
/// install of nothing.
#[test]
fn remediation_is_blocked_without_a_script_or_without_targets() {
    let devices = vec![device(1, "srv-a", 1, false)];
    let ids = [1];
    let names = orgs();

    for kind in [
        ActionKind::OsPatchRemediate,
        ActionKind::SoftwarePatchRemediate,
    ] {
        // No script id configured for this family.
        let bare = ActionSettings::default();
        let p = plan(input(kind, &ids, &devices, &names, &bare));
        assert!(
            p.blockers
                .iter()
                .any(|b| b.contains("No remediation script configured")),
            "{kind:?}: {:?}",
            p.blockers
        );
        // ...and it points at the native apply as the way to proceed today.
        assert!(
            p.blockers
                .iter()
                .any(|b| b.contains(kind.untargeted_counterpart().unwrap().label())),
            "{kind:?}: {:?}",
            p.blockers
        );

        // Script configured, but nothing ticked.
        let configured = with_scripts();
        let p = plan(PlanInput {
            targets: &[],
            ..input(kind, &ids, &devices, &names, &configured)
        });
        assert!(
            p.blockers.iter().any(|b| b.contains("No patches selected")),
            "{kind:?}: {:?}",
            p.blockers
        );

        // Both satisfied — the action is available.
        let p = plan(input(kind, &ids, &devices, &names, &configured));
        assert!(!p.is_blocked(), "{kind:?}: {:?}", p.blockers);
        assert!(
            !p.warnings.iter().any(|w| w.contains("every approved")),
            "{kind:?} installs only what was selected, so the all-patches warning is wrong"
        );
    }
}

/// A script that restarts the device when it finishes is as consequential as the
/// native apply, so the dialog must flag it the same way.
#[test]
fn a_script_that_reboots_is_flagged_as_rebooting() {
    let devices = vec![device(1, "srv-a", 1, false)];
    let ids = [1];
    let names = orgs();
    let settings = with_scripts();

    for kind in [
        ActionKind::Script,
        ActionKind::OsPatchRemediate,
        ActionKind::SoftwarePatchRemediate,
    ] {
        let p = plan(input(kind, &ids, &devices, &names, &settings));
        assert!(!p.reboot_expected, "{kind:?} with rebootBehavior=Never");

        let p = plan(PlanInput {
            reboot: RebootChoice::Auto,
            ..input(kind, &ids, &devices, &names, &settings)
        });
        assert!(p.reboot_expected, "{kind:?} with rebootBehavior=Auto");

        // ...but a preview restarts nothing.
        let p = plan(PlanInput {
            reboot: RebootChoice::Auto,
            dry_run: true,
            ..input(kind, &ids, &devices, &names, &settings)
        });
        assert!(!p.reboot_expected, "{kind:?} dry run");
    }
}

/// Software targets can contain spaces ("Google Chrome"), which NinjaOne would
/// split into separate `key=value` tokens — hence the base64 encoding. This arm
/// was unreachable until `SoftwarePatchRemediate` existed.
#[test]
fn software_targets_are_encoded_and_os_targets_are_a_kb_list() {
    let os = build_parameters(
        ActionKind::OsPatchRemediate,
        &["KB5040434".into(), "5041580".into()],
        RebootChoice::Never,
        false,
    );
    assert_eq!(
        os,
        "kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=false"
    );

    let sw = build_parameters(
        ActionKind::SoftwarePatchRemediate,
        &["Google Chrome".into(), "7-Zip".into()],
        RebootChoice::Auto,
        true,
    );
    assert!(
        sw.starts_with("productAllowListB64="),
        "software targets must not be sent as a bare list: {sw}"
    );
    assert!(
        !sw.split(' ').next().unwrap().contains(' '),
        "the encoded token must be space-free: {sw}"
    );
    assert!(sw.ends_with(" rebootBehavior=Auto dryRun=true"), "{sw}");
}

#[test]
fn dry_run_is_rejected_for_kinds_with_no_preview() {
    let devices = vec![device(1, "srv-a", 1, false)];
    let ids = [1];
    let names = orgs();
    let settings = ActionSettings::default();

    // The native endpoints have no preview, so a "dry run" would send nothing.
    for kind in [
        ActionKind::OsPatchApply,
        ActionKind::SoftwarePatchApply,
        ActionKind::Reboot,
        ActionKind::OsPatchScan,
    ] {
        let p = plan(PlanInput {
            dry_run: true,
            ..input(kind, &ids, &devices, &names, &settings)
        });
        assert!(p.is_blocked(), "{kind:?} must not pretend to preview");
        assert!(p.blockers.iter().any(|b| b.contains("no preview mode")));
    }

    // A script has a real dry run.
    let p = plan(PlanInput {
        dry_run: true,
        ..input(ActionKind::Script, &ids, &devices, &names, &settings)
    });
    assert!(!p.is_blocked());
    assert!(
        !p.reboot_expected,
        "a preview must not claim it will reboot"
    );
}

#[test]
fn plan_reports_unknown_devices_rather_than_dropping_them() {
    let devices = vec![device(1, "srv-a", 1, false)];
    let ids = [1, 99];
    let names = orgs();
    let settings = ActionSettings::default();

    let p = plan(input(
        ActionKind::OsPatchScan,
        &ids,
        &devices,
        &names,
        &settings,
    ));
    assert_eq!(p.eligible.len(), 1);
    assert_eq!(p.skipped.len(), 1);
    assert_eq!(p.skipped[0].device_id, 99);
}

/// The confirm hash de-duplicated the ids while `plan()` and the dispatch loop
/// did not, so an approval for `[5]` validated `[5, 5]` — two runs on one
/// machine. Scans are not exempt: the request itself is malformed.
#[test]
fn a_device_listed_twice_is_a_blocker() {
    let devices = vec![device(5, "srv-a", 1, false), device(6, "srv-b", 1, false)];
    let ids = [5, 6, 5, 5];
    let names = orgs();
    let settings = ActionSettings::default();

    for kind in [ActionKind::Reboot, ActionKind::OsPatchScan] {
        let p = plan(input(kind, &ids, &devices, &names, &settings));
        assert!(
            p.blockers
                .iter()
                .any(|b| b.contains("device(s) 5 more than once")),
            "{kind:?}: {:?}",
            p.blockers
        );
    }
    let p = plan(input(
        ActionKind::Reboot,
        &[5, 6],
        &devices,
        &names,
        &settings,
    ));
    assert!(!p.blockers.iter().any(|b| b.contains("more than once")));
}

/// NinjaOne splits `parameters` on spaces, so a KB target is an injection
/// point: "123 dryRun=false" would add a key of its own to the string. Every
/// KB-encoded kind refuses anything but a KB number; the software kinds send
/// base64 and are unaffected.
#[test]
fn a_target_that_is_not_a_kb_number_blocks_a_kb_encoded_dispatch() {
    let devices = vec![device(1, "srv-a", 1, false)];
    let ids = [1];
    let names = orgs();
    let settings = with_scripts();
    let bad: &[&str] = &["KB5040434", "123 dryRun=false", "kb12,34", ""];

    for kind in [ActionKind::OsPatchRemediate, ActionKind::Script] {
        let p = plan(PlanInput {
            targets: bad,
            ..input(kind, &ids, &devices, &names, &settings)
        });
        let b = p
            .blockers
            .iter()
            .find(|b| b.contains("Not a KB number"))
            .unwrap_or_else(|| panic!("{kind:?}: {:?}", p.blockers));
        assert!(b.contains("\"123 dryRun=false\""), "{b}");
        assert!(b.contains("\"kb12,34\""), "{b}");
        assert!(!b.contains("\"KB5040434\""), "a valid KB is not named: {b}");
    }

    let p = plan(PlanInput {
        targets: &["Google Chrome", "7-Zip 23.01 (x64)"],
        ..input(
            ActionKind::SoftwarePatchRemediate,
            &ids,
            &devices,
            &names,
            &settings,
        )
    });
    assert!(!p.is_blocked(), "{:?}", p.blockers);

    let p = plan(PlanInput {
        targets: &["KB5040434", "kb5041580", "5041581", " Kb1 "],
        ..input(
            ActionKind::OsPatchRemediate,
            &ids,
            &devices,
            &names,
            &settings,
        )
    });
    assert!(!p.is_blocked(), "{:?}", p.blockers);
}

#[test]
fn kb_number_accepts_only_digits_after_an_optional_kb_prefix() {
    for (raw, want) in [
        ("KB5040434", Some("5040434")),
        ("kb5040434", Some("5040434")),
        ("Kb5040434", Some("5040434")),
        (" 5040434 ", Some("5040434")),
        ("KB", None),
        ("", None),
        ("5040434 dryRun=false", None),
        ("KB50404a", None),
        ("KBKB1", None),
        ("١٢٣", None),
    ] {
        assert_eq!(kb_number(raw), want, "{raw:?}");
    }
    // Defense in depth: a malformed target never reaches the string either.
    assert_eq!(
        build_parameters(
            ActionKind::OsPatchRemediate,
            &["5040434 dryRun=false".into(), "kb1".into()],
            RebootChoice::Never,
            true,
        ),
        "kbAllowList=1 rebootBehavior=Never dryRun=true"
    );
}

#[test]
fn build_parameters_sets_dry_run_flag() {
    let targets = vec!["KB5040434".to_string(), "5041580".to_string()];
    assert_eq!(
        build_parameters(
            ActionKind::OsPatchApply,
            &targets,
            RebootChoice::Never,
            true
        ),
        "kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=true"
    );
    assert_eq!(
        build_parameters(
            ActionKind::OsPatchApply,
            &targets,
            RebootChoice::Never,
            false
        ),
        "kbAllowList=5040434,5041580 rebootBehavior=Never dryRun=false"
    );
}

#[test]
fn build_parameters_reflects_reboot_choice() {
    let targets = vec!["KB1".to_string()];
    assert_eq!(
        build_parameters(
            ActionKind::OsPatchApply,
            &targets,
            RebootChoice::Auto,
            false
        ),
        "kbAllowList=1 rebootBehavior=Auto dryRun=false"
    );
}

#[test]
fn build_parameters_software_encodes_product_allow_list() {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let targets = vec!["Google Chrome".to_string(), "7-Zip".to_string()];
    let params = build_parameters(
        ActionKind::SoftwarePatchApply,
        &targets,
        RebootChoice::Auto,
        false,
    );

    let encoded = params
        .strip_prefix("productAllowListB64=")
        .and_then(|s| s.split(' ').next())
        .expect("encoded token present");
    let decoded = STANDARD.decode(encoded).expect("valid base64");
    assert_eq!(String::from_utf8(decoded).unwrap(), "Google Chrome|7-Zip");
    // The whole point: no spaces leak into what NinjaOne tokenizes.
    assert!(!encoded.contains(' '));
    assert!(params.ends_with(" rebootBehavior=Auto dryRun=false"));
}

/// The reference scripts in `remediation/` parse exactly these strings, and their
/// Pester suite reads the same fixture — so a change to the encoding here fails
/// this test until the fixture, and with it the scripts' tests, are updated too.
#[test]
fn build_parameters_matches_the_reference_script_fixture() {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        name: String,
        kind: ActionKind,
        targets: Vec<String>,
        reboot: RebootChoice,
        dry_run: bool,
        parameters: String,
    }
    #[derive(Deserialize)]
    struct Fixture {
        cases: Vec<Case>,
    }
    let fixture: Fixture = serde_json::from_str(include_str!(
        "../../../remediation/tests/fixtures/parameter-contract.json"
    ))
    .expect("fixture parses");

    for case in &fixture.cases {
        assert_eq!(
            build_parameters(case.kind, &case.targets, case.reboot, case.dry_run),
            case.parameters,
            "{}",
            case.name
        );
    }
    // Every value of the `rebootBehavior` vocabulary and both encodings are pinned.
    for reboot in [RebootChoice::Never, RebootChoice::Auto] {
        assert!(
            fixture.cases.iter().any(|c| c.reboot == reboot),
            "{reboot:?} missing from the fixture"
        );
    }
    for kind in [
        ActionKind::OsPatchRemediate,
        ActionKind::SoftwarePatchRemediate,
    ] {
        assert!(
            fixture.cases.iter().any(|c| c.kind == kind),
            "{kind:?} missing from the fixture"
        );
    }
}

fn job(activity_id: Option<i64>, series_uid: Option<&str>, dispatched_ts: i64) -> JobReport {
    JobReport {
        id: 1,
        batch_id: 1,
        device_id: 42,
        device_name: "srv-1".into(),
        organization: "Contoso".into(),
        kind: ActionKind::Script,
        detail: "Install-CriticalSecurityUpdates".into(),
        dry_run: false,
        state: JobState::Running,
        dispatched_at: fmt_ts(Utc::now()),
        dispatched_ts,
        finished_at: None,
        duration_seconds: None,
        activity_id,
        series_uid: series_uid.map(str::to_string),
        exit_code: None,
        request: None,
    }
}

fn activity(v: serde_json::Value) -> Activity {
    serde_json::from_value(v).expect("activity")
}

#[test]
fn match_activity_prefers_activity_id() {
    let now = Utc::now().timestamp();
    let j = job(Some(1002), None, now);
    let list = vec![
        activity(
            json!({ "id": 1001, "activityType": "SCRIPT", "status": "COMPLETED",
                         "activityTime": now as f64 }),
        ),
        activity(
            json!({ "id": 1002, "activityType": "SCRIPT", "status": "RUNNING",
                         "activityTime": (now - 2) as f64 }),
        ),
    ];
    assert_eq!(
        match_activity(&list, &j, &HashSet::new()).and_then(|a| a.id),
        Some(1002),
        "the exact id must win over the newer row"
    );
}

#[test]
fn match_activity_falls_back_to_the_series_uid() {
    let now = Utc::now().timestamp();
    let j = job(None, Some("uid-abc"), now);
    let list = vec![
        activity(json!({ "id": 1, "activityType": "SCRIPT", "activityTime": now as f64 })),
        activity(
            json!({ "id": 2, "seriesUid": "uid-abc", "activityType": "SCRIPT",
                         "activityTime": (now - 30) as f64 }),
        ),
    ];
    assert_eq!(
        match_activity(&list, &j, &HashSet::new()).and_then(|a| a.id),
        Some(2)
    );
}

/// The native endpoints return no correlator, so the activity-type heuristic is
/// their only path to resolving — and each kind accepts exactly the types it
/// emits, not every type any kind might. A single shared list let a `SYSTEM`
/// event resolve a script, and a software apply resolve an OS apply.
#[test]
fn each_kind_matches_only_the_activity_types_it_emits() {
    let now = Utc::now().timestamp();
    let script_types = &["SCRIPTING", "SCRIPT", "ACTION", "ACTIONSET"][..];
    let all = [
        "PATCH_MANAGEMENT",
        "SOFTWARE_PATCH_MANAGEMENT",
        "SYSTEM",
        "SCRIPTING",
        "SCRIPT",
        "ACTION",
        "ACTIONSET",
        // NinjaOne's own policy and scheduler runs: never a dispatched job's.
        "CONDITION_ACTION",
        "CONDITION_ACTIONSET",
        "SCHEDULED_TASK",
        "SPLASHTOP_CONNECTION_INITIATED",
    ];
    for (kind, accepted) in [
        (ActionKind::OsPatchScan, &["PATCH_MANAGEMENT"][..]),
        (ActionKind::OsPatchApply, &["PATCH_MANAGEMENT"][..]),
        (
            ActionKind::SoftwarePatchScan,
            &["SOFTWARE_PATCH_MANAGEMENT"][..],
        ),
        (
            ActionKind::SoftwarePatchApply,
            &["SOFTWARE_PATCH_MANAGEMENT"][..],
        ),
        (ActionKind::Reboot, &["SYSTEM"][..]),
        (ActionKind::Script, script_types),
        (ActionKind::OsPatchRemediate, script_types),
        (ActionKind::SoftwarePatchRemediate, script_types),
    ] {
        let j = JobReport {
            kind,
            ..job(None, None, now)
        };
        for t in all {
            let list = vec![activity(
                json!({ "id": 1, "activityType": t, "activityTime": now as f64 }),
            )];
            assert_eq!(
                match_activity(&list, &j, &HashSet::new()).is_some(),
                accepted.contains(&t),
                "{kind:?} vs {t}"
            );
        }
    }
}

/// An OS apply and a software apply to one device, back to back: neither has a
/// correlator, and each must land on its own family's activity.
#[test]
fn back_to_back_os_and_software_applies_resolve_to_their_own_activities() {
    let now = Utc::now();
    let ts = now.timestamp();
    let list = vec![
        activity(json!({
            "id": 502, "activityType": "SOFTWARE_PATCH_MANAGEMENT",
            "activityTime": ts as f64,
            "statusCode": "COMPLETED", "activityResult": "FAILURE",
        })),
        activity(json!({
            "id": 501, "activityType": "PATCH_MANAGEMENT",
            "activityTime": (ts - 2) as f64,
            "statusCode": "COMPLETED", "activityResult": "SUCCESS",
        })),
    ];
    let mut claimed = HashSet::new();
    let mut os = JobReport {
        kind: ActionKind::OsPatchApply,
        ..job(None, None, ts - 10)
    };
    let mut sw = JobReport {
        id: 2,
        kind: ActionKind::SoftwarePatchApply,
        ..job(None, None, ts - 9)
    };
    advance_job(&mut os, &list, now, &mut claimed);
    advance_job(&mut sw, &list, now, &mut claimed);

    assert_eq!(os.activity_id, Some(501));
    assert_eq!(os.state, JobState::Completed);
    assert_eq!(sw.activity_id, Some(502));
    assert!(matches!(sw.state, JobState::Failed(_)), "{:?}", sw.state);
}

/// `statusCode` is the enumerated lifecycle and `activityResult` is the outcome;
/// a COMPLETED activity carrying FAILURE is a failed job, not a successful one.
#[test]
fn the_outcome_comes_from_activity_result_not_the_lifecycle_code() {
    let now = Utc::now();
    let ts = now.timestamp();
    let mut j = job(Some(7), None, ts);
    let list = vec![activity(json!({
        "id": 7, "activityType": "SCRIPTING", "activityTime": ts as f64,
        "statusCode": "COMPLETED", "activityResult": "FAILURE",
        "data": { "exitCode": 1 }
    }))];
    advance_job(&mut j, &list, now, &mut HashSet::new());
    assert_eq!(j.state, JobState::Failed("FAILURE".into()));
    assert_eq!(j.exit_code, Some(1));

    // …and a genuine success still reads as one.
    let mut ok = job(Some(8), None, ts);
    let good = vec![activity(json!({
        "id": 8, "activityType": "SCRIPTING", "activityTime": ts as f64,
        "statusCode": "COMPLETED", "activityResult": "SUCCESS",
        "data": { "exitCode": 0 }
    }))];
    advance_job(&mut ok, &good, now, &mut HashSet::new());
    assert_eq!(ok.state, JobState::Completed);
    assert_eq!(ok.exit_code, Some(0));
}

#[test]
fn match_activity_ignores_rows_predating_the_dispatch() {
    let now = Utc::now().timestamp();
    let j = job(None, None, now);
    let stale = vec![activity(
        json!({ "id": 9, "activityType": "SCRIPT", "activityTime": (now - 600) as f64 }),
    )];
    assert!(
        match_activity(&stale, &j, &HashSet::new()).is_none(),
        "a run from ten minutes ago is not this job"
    );
}

#[test]
fn advance_marks_timeout_only_when_older_than_the_threshold() {
    let now = Utc::now();

    // Nothing matched yet, but still inside the window: hold state.
    let mut fresh = job(None, None, now.timestamp() - 60);
    advance_job(&mut fresh, &[], now, &mut HashSet::new());
    assert_eq!(fresh.state, JobState::Running);
    assert!(fresh.finished_at.is_none());

    let mut old = job(None, None, now.timestamp() - (JOB_TIMEOUT_MINUTES + 1) * 60);
    advance_job(&mut old, &[], now, &mut HashSet::new());
    assert_eq!(old.state, JobState::TimedOut);
    assert!(old.finished_at.is_some());
}

/// A transient failure to *read* the activity feed must never be recorded as a
/// failure of the job itself — the reference implementation did this, and it
/// reported healthy patch runs as failures whenever the API hiccupped.
#[test]
fn an_empty_poll_does_not_fail_the_job() {
    let now = Utc::now();
    let mut j = job(None, None, now.timestamp());
    advance_job(&mut j, &[], now, &mut HashSet::new());
    assert_eq!(j.state, JobState::Running);
    assert!(!j.state.is_terminal());
}

#[test]
fn advance_captures_exit_code_and_correlators_on_completion() {
    let now = Utc::now();
    let mut j = job(None, None, now.timestamp());
    let list = vec![activity(json!({
        "id": 77, "seriesUid": "uid-z", "activityType": "SCRIPT",
        "status": "COMPLETED", "activityTime": now.timestamp() as f64,
        "result": { "exitCode": 2 },
    }))];
    advance_job(&mut j, &list, now, &mut HashSet::new());

    assert_eq!(j.state, JobState::Completed);
    assert_eq!(j.exit_code, Some(2));
    assert_eq!(j.activity_id, Some(77));
    assert_eq!(j.series_uid.as_deref(), Some("uid-z"));
    assert!(j.duration_seconds.is_some());
}

/// A device that keeps producing matching *non-terminal* activities used to hold
/// the job at Running forever: only the `None` and poll-error arms checked the
/// timeout. Such a row pinned the poller alive and could never be evicted by
/// `append_jobs`'s MAX_JOBS trim, which retains non-terminal jobs.
#[test]
fn a_perpetually_running_activity_still_times_out() {
    let now = Utc::now();
    let dispatched = now.timestamp() - (JOB_TIMEOUT_MINUTES + 1) * 60;
    let mut j = job(None, None, dispatched);
    let list = vec![activity(json!({
        "id": 5, "activityType": "SCRIPT", "status": "RUNNING",
        "activityTime": dispatched as f64,
    }))];

    advance_job(&mut j, &list, now, &mut HashSet::new());

    assert_eq!(j.state, JobState::TimedOut);
    assert!(j.state.is_terminal(), "or it pins the poller forever");
}

/// Still inside the window, the same match keeps the job Running — the timeout
/// above must not swallow a healthy long run.
#[test]
fn a_running_activity_inside_the_window_stays_running() {
    let now = Utc::now();
    let mut j = job(None, None, now.timestamp() - 60);
    let list = vec![activity(json!({
        "id": 5, "activityType": "SCRIPT", "status": "RUNNING",
        "activityTime": (now.timestamp() - 60) as f64,
    }))];

    advance_job(&mut j, &list, now, &mut HashSet::new());

    assert_eq!(j.state, JobState::Running);
    assert_eq!(
        j.activity_id,
        Some(5),
        "correlation is recorded on the first match, so later polls use tier 1 \
         instead of re-running the heuristic and possibly landing elsewhere"
    );
}

/// The native endpoints (scan/apply/reboot) return no correlator, so every one of
/// those jobs reaches the third-tier "newest matching activity on this device"
/// heuristic. Without an exclusion set, two actions dispatched to the same device
/// close together both select the newest activity and swap exit codes.
#[test]
fn two_jobs_on_one_device_cannot_claim_the_same_activity() {
    let now = Utc::now();
    let ts = now.timestamp();
    let list = vec![
        activity(json!({
            "id": 200, "activityType": "ACTION", "status": "COMPLETED",
            "activityTime": ts as f64, "result": { "exitCode": 0 },
        })),
        activity(json!({
            "id": 199, "activityType": "ACTION", "status": "COMPLETED",
            "activityTime": (ts - 1) as f64, "result": { "exitCode": 3 },
        })),
    ];

    let mut claimed = HashSet::new();
    let mut first = job(None, None, ts - 10);
    let mut second = job(None, None, ts - 10);

    advance_job(&mut first, &list, now, &mut claimed);
    advance_job(&mut second, &list, now, &mut claimed);

    assert_eq!(
        first.activity_id,
        Some(200),
        "newest wins for the first job"
    );
    assert_eq!(
        second.activity_id,
        Some(199),
        "the second must fall to the next unclaimed activity, not re-take 200"
    );
    assert_ne!(
        first.exit_code, second.exit_code,
        "distinct activities carry their own exit codes"
    );
}

/// The exclusion applies only to the heuristic tier. A job that already owns an
/// activity id must keep resolving to it on every later poll, even though that id
/// is by then in the claimed set.
#[test]
fn an_exact_id_match_ignores_the_claimed_set() {
    let now = Utc::now();
    let j = job(Some(42), None, now.timestamp());
    let list = vec![activity(json!({
        "id": 42, "activityType": "SCRIPT", "status": "RUNNING",
        "activityTime": now.timestamp() as f64,
    }))];
    let claimed = HashSet::from([42]);

    assert_eq!(
        match_activity(&list, &j, &claimed).and_then(|a| a.id),
        Some(42),
        "its own claim must not lock a job out of its own activity"
    );
}

/// `Unknown` must stay non-terminal: the dispatch may already be running on the
/// device, so the poller has to keep trying to correlate it rather than closing
/// the row out.
#[test]
fn unknown_is_not_terminal_but_the_other_end_states_are() {
    assert!(!JobState::Unknown("timeout".into()).is_terminal());
    assert!(!JobState::Queued.is_terminal());
    assert!(!JobState::Running.is_terminal());
    assert!(JobState::Completed.is_terminal());
    assert!(JobState::Failed("boom".into()).is_terminal());
    assert!(JobState::TimedOut.is_terminal());
    assert!(JobState::Skipped("offline".into()).is_terminal());
}

/// `ActionKind::ALL` is the backend half of the IPC fixture's `actionKind` set,
/// which the frontend's mirror must serialize to exactly — so a kind missing from
/// it is a kind the frontend is never checked against. A new variant breaks this
/// match until it is given a slot in `ALL`.
#[test]
fn all_lists_every_action_kind_once() {
    let slot = |k: ActionKind| match k {
        ActionKind::OsPatchScan => 0,
        ActionKind::SoftwarePatchScan => 1,
        ActionKind::OsPatchApply => 2,
        ActionKind::SoftwarePatchApply => 3,
        ActionKind::OsPatchRemediate => 4,
        ActionKind::SoftwarePatchRemediate => 5,
        ActionKind::Reboot => 6,
        ActionKind::Script => 7,
    };
    for (i, k) in ActionKind::ALL.into_iter().enumerate() {
        assert_eq!(slot(k), i, "{k:?}");
    }
}

/// Same guard for `JobState::ALL`, which supplies the fixture's `jobState` set and
/// one Jobs row per state.
#[test]
fn all_lists_every_job_state_once() {
    let slot = |s: &JobState| match s {
        JobState::Queued => 0,
        JobState::Running => 1,
        JobState::Completed => 2,
        JobState::Failed(_) => 3,
        JobState::TimedOut => 4,
        JobState::Unknown(_) => 5,
        JobState::Skipped(_) => 6,
    };
    for (i, s) in JobState::ALL.iter().enumerate() {
        assert_eq!(slot(s), i, "{s:?}");
    }
}

/// The tagged representation the frontend switches on. A `Failed` row must
/// carry its message in `detail`, or the Jobs tab shows a bare "Failed".
#[test]
fn job_state_serializes_as_a_tagged_state_plus_detail() {
    let plain = serde_json::to_value(JobState::Completed).expect("serialize");
    assert_eq!(plain["state"], "completed");

    let failed = serde_json::to_value(JobState::Failed("boom".into())).expect("serialize");
    assert_eq!(failed["state"], "failed");
    assert_eq!(failed["detail"], "boom");

    let skipped = serde_json::to_value(JobState::Skipped("offline".into())).expect("serialize");
    assert_eq!(skipped["state"], "skipped");
    assert_eq!(skipped["detail"], "offline");
}

#[test]
fn scans_are_not_mutating_but_everything_else_is() {
    assert!(!ActionKind::OsPatchScan.is_mutating());
    assert!(!ActionKind::SoftwarePatchScan.is_mutating());
    for k in [
        ActionKind::OsPatchApply,
        ActionKind::SoftwarePatchApply,
        ActionKind::Reboot,
        ActionKind::Script,
    ] {
        assert!(k.is_mutating(), "{k:?} changes the device");
    }
}
