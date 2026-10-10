//! Fixtures the backend generates and `web-rs` asserts against.
//!
//! The two crates share no code, so every frontend mirror of a backend shape is
//! held to it only by a test. Each fixture is the backend's real output for a fixed
//! input, committed under `web-rs/tests/`: the generator here fails when the
//! committed copy is stale, and a frontend test fails when its mirror no longer
//! reads what the file says. Neither half can move alone.
//!
//! Regenerate both deliberately with `UPDATE_FIXTURES=1 cargo test --manifest-path
//! src-tauri/Cargo.toml fixture_is_current`; the diff is the wire change to review.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};

use crate::actions::{
    ActionKind, ActionPlan, CachedFamily, JobReport, JobRequest, JobState, PlannedTarget,
    RebootChoice, SkippedTarget, apply_preview, fmt_ts,
};
use crate::changes::{self, RunSnapshot};
use crate::commands::actions::{ActionBatch, ActionProgressEvent, RunAsOptions, ScriptSummary};
use crate::commands::auth::AuthStatus;
use crate::commands::diagnostics::AuditRecord;
use crate::commands::lookups::list_node_classes;
use crate::commands::patches::QueryProgressEvent;
use crate::commands::settings::SettingsView;
use crate::commands::update::UpdateInfo;
use crate::filter::FilterParams;
use crate::history::RunRecord;
use crate::model::{Device, Location, Organization, OsInfo, Patch, PatchStatus, RebootMode, Role};
use crate::rows::{
    GroupBy, InstallWindow, LookupMaps, PatchFamilies, PatchSource, QueryResult, QuerySummary,
    SlaCutoffs, apply_device_health, build_age_buckets, build_approval_backlog, build_compliance,
    build_compliance_by_os, build_device_backlogs, build_device_summaries, build_failures,
    build_groups, build_query_scope, build_rows, build_severity_by_org, build_time_to_install,
    device_detail, page_rows, pending_counts, rows_by_device, slice_groups, sort_device_summaries,
};
use crate::settings::{ActionSettings, Preset, SlaBySeverity, SlaPolicy};

/// Writes `fixture` to `path` under `UPDATE_FIXTURES`, and otherwise fails unless the
/// committed file holds exactly what `fixture` renders to. `path` is relative to
/// `src-tauri/`, where `cargo test` runs.
pub(crate) fn assert_fixture_current(path: &str, fixture: &Value) {
    let rendered = format!(
        "{}\n",
        serde_json::to_string_pretty(fixture).expect("serialize the fixture")
    );
    if std::env::var("UPDATE_FIXTURES").is_ok() {
        std::fs::write(path, &rendered).expect("write the fixture");
        return;
    }
    // Normalized before comparing: a Windows checkout can hand this back with CRLF
    // (`.gitattributes` pins the file to LF, but a clone predating that — or a
    // core.autocrlf override — still would). The fixture's *content* is what this
    // asserts, and a line ending is not content. Failing on it would make the gate
    // noise on one platform, which teaches everyone to ignore it.
    let committed = std::fs::read_to_string(path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    if committed != rendered {
        // The first differing line, not two whole files: these run to thousands of
        // lines, and `assert_eq!` would print both.
        let (line, was, now) = committed
            .lines()
            .zip(rendered.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b)
            .map(|(i, (a, b))| (i + 1, a, b))
            .unwrap_or((
                committed.lines().count().min(rendered.lines().count()) + 1,
                "",
                "",
            ));
        panic!(
            "{path} is stale from line {line} (committed `{was}`, generated `{now}`). The \
             backend's output changed, so the web-rs code asserting against it must change \
             with it. Regenerate with UPDATE_FIXTURES=1 cargo test --manifest-path \
             src-tauri/Cargo.toml fixture_is_current"
        );
    }
}

/// Path of the IPC fixture, relative to `src-tauri/`.
const IPC_FIXTURE: &str = "../web-rs/tests/backend-ipc.json";

/// The one clock every value in the IPC fixture is computed against (2026-07-01
/// 12:00:00 UTC). A real clock read anywhere in the inputs would make the committed
/// file stale by the next run.
const FIXTURE_NOW: DateTime<Utc> =
    DateTime::from_timestamp(1_782_907_200, 0).expect("a valid fixture instant");

const INSTANCE: &str = "https://app.ninjarmm.com";
const ORG_ID: i64 = 10;
const LOCATION_ID: i64 = 100;
const ROLE_ID: i64 = 2;
const BATCH_ID: u64 = 7;

/// `days` before [`FIXTURE_NOW`], as the epoch seconds a NinjaOne record carries.
fn days_ago(days: i64) -> f64 {
    (FIXTURE_NOW - Duration::days(days)).timestamp() as f64
}

/// A device in the fixture's one organization, location and role. Every one is
/// flagged for reboot, so all three rollup scopes ride on the summary's reboot list.
fn device(id: i64, name: &str, class: &str, os: &str, offline: bool) -> Device {
    Device {
        id,
        system_name: Some(name.into()),
        display_name: Some(name.into()),
        organization_id: Some(ORG_ID),
        location_id: Some(LOCATION_ID),
        node_role_id: Some(ROLE_ID),
        node_class: Some(class.into()),
        offline: Some(offline),
        os: Some(OsInfo {
            name: Some(os.into()),
            needs_reboot: Some(true),
        }),
        last_contact: Some(days_ago(1)),
    }
}

/// A feed record on `device_id`, first seen `seen_days` before [`FIXTURE_NOW`].
fn patch_at(
    device_id: i64,
    kb: Option<&str>,
    name: &str,
    severity: &str,
    status: &str,
    seen_days: i64,
) -> Patch {
    Patch {
        device_id: Some(device_id),
        kb_number: kb.map(Into::into),
        name: Some(name.into()),
        version: None,
        product_vendor: None,
        severity: Some(severity.into()),
        status: Some(status.into()),
        patch_type: None,
        collected_timestamp: Some(days_ago(seen_days)),
        installed_timestamp: None,
        product_identifier: None,
    }
}

/// Fails unless every field of every shape in the fixture has at least one sample
/// that is neither `null` nor `[]`. A field that is always `None` or empty is a
/// nested shape the frontend is never checked against, so a new `Option`/`Vec`
/// field cannot join the wire without a filled sample here.
///
/// A shape is identified by its key set — a stand-in for the Rust type — so a field
/// counts as filled if any instance fills it: a two-row page need not repeat what
/// the summary's rows already show.
fn assert_every_field_is_exercised(fixture: &Value) {
    type Filled = BTreeMap<(String, String), (bool, String)>;
    fn walk(path: &str, value: &Value, filled: &mut Filled) {
        match value {
            Value::Object(fields) => {
                let shape = fields.keys().cloned().collect::<Vec<_>>().join(",");
                for (key, v) in fields {
                    let at = format!("{path}.{key}");
                    let has_data = !(v.is_null() || v.as_array().is_some_and(Vec::is_empty));
                    filled
                        .entry((shape.clone(), key.clone()))
                        .or_insert((false, at.clone()))
                        .0 |= has_data;
                    walk(&at, v, filled);
                }
            }
            Value::Array(items) => {
                for (i, v) in items.iter().enumerate() {
                    walk(&format!("{path}[{i}]"), v, filled);
                }
            }
            _ => {}
        }
    }
    let mut filled = Filled::new();
    walk("", fixture, &mut filled);
    let gaps: Vec<&str> = filled
        .values()
        .filter(|(has_data, _)| !has_data)
        .map(|(_, example)| example.as_str())
        .collect();
    assert!(
        gaps.is_empty(),
        "these fixture fields are never filled, so the frontend decodes them only as \
         null or []: {gaps:?}. Give each a Some / non-empty sample."
    );
}

/// Emits `web-rs/tests/backend-ipc.json` — one value for every type the frontend
/// decodes off IPC, keyed by the command (or event) that returns it — and fails
/// when the committed copy is out of date.
///
/// `web-rs/src/types.rs` hand-mirrors these shapes, and the hand-typed key lists
/// that used to guard them never decoded anything through the mirror: a type change,
/// an enum spelling, or a rename hidden by `#[serde(default)]` surfaced only as a
/// "decode <cmd>" toast — or, for the two progress events, as nothing at all. The
/// frontend's `types::tests` now decodes every entry with its real mirror type and
/// checks that every key the mirror reads is one the backend sent.
///
/// Rules that keep it useful: every clock read is [`FIXTURE_NOW`]; every id, token
/// and version is a literal; every `Option` is `Some` and every `Vec` non-empty
/// somewhere ([`assert_every_field_is_exercised`]), even where the backend never
/// sends both at once (a blocked plan carries no token). What it cannot see: a
/// backend `T` → `Option<T>` change regenerates an identical file while every
/// sample is `Some`, so a field made optional needs a `None` sample added too.
#[test]
fn ipc_fixture_is_current() {
    let orgs = vec![Organization {
        id: ORG_ID,
        name: "Contoso".into(),
    }];
    let locations = vec![Location {
        id: LOCATION_ID,
        name: "HQ".into(),
        organization_id: Some(ORG_ID),
    }];
    let roles = vec![Role {
        id: ROLE_ID,
        name: "Domain Controller".into(),
        node_class: Some("WINDOWS_SERVER".into()),
    }];
    let maps = LookupMaps::build(&orgs, &locations, &roles);

    // One device per `RollupScope`: included, offline, and online but unpatchable.
    let devices = [
        device(1, "web-01", "WINDOWS_SERVER", "Windows Server 2022", false),
        device(2, "web-02", "WINDOWS_SERVER", "Windows Server 2022", true),
        device(3, "core-sw-01", "NMS_SWITCH", "Cisco IOS XE", false),
    ];
    let device_refs: Vec<&Device> = devices.iter().collect();
    let by_id: HashMap<i64, &Device> = devices.iter().map(|d| (d.id, d)).collect();

    // Pending past its 14-day band; approved and stuck past the 30-day default; and
    // the offline device's backlog. Every distinct (device, patch) once, so no
    // hash-ordered tie can reorder the output.
    let os_current = [
        patch_at(
            1,
            Some("KB5040434"),
            "Cumulative Update",
            "CRITICAL",
            "MANUAL",
            45,
        ),
        patch_at(
            1,
            Some("KB5039212"),
            "Security Update",
            "IMPORTANT",
            "APPROVED",
            90,
        ),
        patch_at(
            2,
            Some("KB5040434"),
            "Cumulative Update",
            "CRITICAL",
            "MANUAL",
            10,
        ),
    ];
    let sw_current = [Patch {
        product_identifier: Some("chrome-uuid".into()),
        ..patch_at(
            1,
            None,
            "Google Chrome 138.0.7204.50",
            "RECOMMENDED",
            "MANUAL",
            5,
        )
    }];
    let installs = [
        Patch {
            installed_timestamp: Some(days_ago(2)),
            ..patch_at(
                1,
                Some("KB5038888"),
                "Servicing Stack Update",
                "CRITICAL",
                "INSTALLED",
                20,
            )
        },
        Patch {
            installed_timestamp: Some(days_ago(1)),
            ..patch_at(
                1,
                Some("KB5037777"),
                "Monthly Rollup",
                "CRITICAL",
                "FAILED",
                15,
            )
        },
    ];
    let os_refs: Vec<&Patch> = os_current.iter().collect();
    let sw_refs: Vec<&Patch> = sw_current.iter().collect();
    let install_refs: Vec<&Patch> = installs.iter().collect();
    let all_current: Vec<&Patch> = os_current.iter().chain(&sw_current).collect();
    let failed: Vec<&Patch> = installs
        .iter()
        .filter(|p| p.status.as_deref() == Some(PatchStatus::Failed.api_value()))
        .collect();

    // Labelled the way `query_patches` labels each feed: a current record is pending
    // unless it says otherwise, an install record installed.
    fn source<'a>(
        patches: &'a [&'a Patch],
        type_label: &'static str,
        status: PatchStatus,
    ) -> PatchSource<'a> {
        PatchSource {
            patches,
            type_label,
            status_override: Some(status.api_value()),
            status_filter: None,
        }
    }
    let filter = FilterParams::default();
    let prepared = filter.prepare();
    let rows = build_rows(
        &by_id,
        &maps,
        &[
            source(&os_refs, "OS", PatchStatus::Pending),
            source(&sw_refs, "SOFTWARE", PatchStatus::Pending),
            source(&install_refs, "OS", PatchStatus::Installed),
        ],
        &prepared,
    );

    let sla_policy = SlaPolicy {
        default_days: 30,
        by_severity: SlaBySeverity {
            critical: Some(14),
            important: Some(30),
            security: Some(30),
            moderate: Some(60),
            recommended: Some(60),
            low: Some(90),
            optional: Some(90),
        },
    };
    let cutoffs = SlaCutoffs::new(&sla_policy, FIXTURE_NOW);
    let mut summaries = build_device_summaries(&device_refs, &pending_counts(&all_current), &maps);
    apply_device_health(&mut summaries, &all_current, Some(&failed), &cutoffs);
    sort_device_summaries(&mut summaries);
    let (worst_devices, offline_backlog) =
        build_device_backlogs(&all_current, &by_id, &maps, &cutoffs);
    let families = PatchFamilies {
        os: true,
        software: true,
    };
    let statuses = [
        PatchStatus::Pending,
        PatchStatus::Approved,
        PatchStatus::Installed,
        PatchStatus::Failed,
    ];
    let scope = build_query_scope(
        &filter,
        &maps,
        families,
        &statuses,
        Some(InstallWindow {
            after: days_ago(30) as i64,
            before: None,
            relative_days: Some(30),
        }),
    );

    // The previous run listed one patch this run no longer does (resolved), and
    // neither of this run's pending nor failed ones (new and newly failed).
    let generated_at = fmt_ts(FIXTURE_NOW);
    let resolved = [patch_at(
        1,
        Some("KB5036666"),
        "Preview Update",
        "MODERATE",
        "MANUAL",
        60,
    )];
    let previous_rows = build_rows(
        &by_id,
        &maps,
        &[source(
            &resolved.iter().collect::<Vec<_>>(),
            "OS",
            PatchStatus::Pending,
        )],
        &prepared,
    );
    let tenant = format!("{INSTANCE}|fixture-client-id");
    let previous = RunSnapshot::build(
        &previous_rows,
        &tenant,
        scope.fingerprint.clone(),
        &fmt_ts(FIXTURE_NOW - Duration::days(1)),
        &statuses,
    );
    let current = RunSnapshot::build(
        &rows,
        &tenant,
        scope.fingerprint.clone(),
        &generated_at,
        &statuses,
    );

    let result = QueryResult {
        compliance: build_compliance(&summaries, &all_current, &by_id, &maps, &cutoffs),
        compliance_by_os: build_compliance_by_os(&summaries, &all_current, &by_id, &cutoffs),
        failures: build_failures(&rows),
        severity_by_org: build_severity_by_org(&all_current, &by_id, &maps),
        age_buckets: build_age_buckets(&all_current, &by_id, FIXTURE_NOW),
        time_to_install: build_time_to_install(&rows, true),
        approvals: build_approval_backlog(
            &all_current,
            &by_id,
            &maps,
            sla_policy.default_days,
            FIXTURE_NOW,
        ),
        changes: changes::diff(Some(&previous), &current),
        devices_total: devices.len(),
        devices_offline: devices.iter().filter(|d| d.is_offline()).count(),
        devices_unpatchable: devices
            .iter()
            .filter(|d| !d.is_offline() && !d.is_patchable())
            .count(),
        rows,
        devices: summaries,
        worst_devices,
        offline_backlog,
        sla_policy,
        instance: INSTANCE.into(),
        patch_families: families,
        scope,
        generated_at,
        data_fetched_at: fmt_ts(FIXTURE_NOW - Duration::minutes(10)),
    };

    // Shapes that come from commands rather than the query result.
    let eligible = vec![PlannedTarget {
        device_id: 1,
        device_name: "web-01".into(),
        organization: "Contoso".into(),
        offline: false,
    }];
    let plan = ActionPlan {
        summary: "Apply all OS patches on 1 device in Contoso.".into(),
        apply_preview: apply_preview(
            ActionKind::OsPatchApply,
            &eligible,
            Some(CachedFamily {
                patches: &os_current,
                fetched_at: FIXTURE_NOW - Duration::minutes(10),
            }),
        ),
        eligible,
        skipped: vec![SkippedTarget {
            device_id: 2,
            device_name: "web-02".into(),
            reason: "offline".into(),
        }],
        organizations: vec!["Contoso".into()],
        warnings: vec!["Installs every approved OS patch, not only the ticked rows.".into()],
        blockers: vec!["The maintenance window is closed.".into()],
        reboot_expected: true,
        dry_run: false,
        parameters_preview: Some("kbAllowList=KB5040434".into()),
        window_overridden: true,
        confirm_token: Some("fixture-confirm-token".into()),
    };

    // One job per kind, cycling through every state, with both reboot choices and
    // both reboot modes.
    let jobs: Vec<JobReport> = ActionKind::ALL
        .into_iter()
        .zip(JobState::ALL.into_iter().cycle())
        .enumerate()
        .map(|(i, (kind, state))| {
            let alternate = i % 2 == 1;
            JobReport {
                id: i as u64 + 1,
                batch_id: BATCH_ID,
                device_id: 1,
                device_name: "web-01".into(),
                organization: "Contoso".into(),
                kind,
                detail: kind.label().into(),
                dry_run: kind.supports_dry_run(),
                state,
                dispatched_at: fmt_ts(FIXTURE_NOW - Duration::minutes(10)),
                dispatched_ts: (FIXTURE_NOW - Duration::minutes(10)).timestamp(),
                finished_at: Some(fmt_ts(FIXTURE_NOW - Duration::minutes(5))),
                duration_seconds: Some(300),
                activity_id: Some(9_000 + i as i64),
                series_uid: Some(format!("series-{i}")),
                exit_code: Some(0),
                request: Some(JobRequest {
                    script_id: Some(42),
                    script_uid: Some("3f6c2a5e-script-uid".into()),
                    script_name: Some("Install-SelectedPatches".into()),
                    parameters: Some("kbAllowList=KB5040434".into()),
                    run_as: Some("system".into()),
                    reboot: if alternate {
                        RebootChoice::Auto
                    } else {
                        RebootChoice::Never
                    },
                    reboot_mode: Some(if alternate {
                        RebootMode::Forced
                    } else {
                        RebootMode::Normal
                    }),
                    reason: Some("Patch Tuesday".into()),
                    include_offline: alternate,
                    targets: vec!["KB5040434".into()],
                }),
            }
        })
        .collect();

    let settings = SettingsView {
        instance_base_url: INSTANCE.into(),
        client_id: Some("fixture-client-id".into()),
        callback_port: 8765,
        install_window_days: 30,
        sla_days: 30,
        sla_by_severity: sla_policy.by_severity,
        has_client_secret: true,
        presets: vec![Preset {
            name: "Critical servers".into(),
            filter: FilterParams {
                organization_ids: vec![ORG_ID],
                location_ids: vec![LOCATION_ID],
                role_ids: vec![ROLE_ID],
                node_classes: vec!["WINDOWS_SERVER".into()],
                os_name_contains: Some("2022".into()),
                search: Some("KB5040434".into()),
                severities: vec!["CRITICAL".into()],
                detected_within_days: Some(30),
                detected_after: Some(days_ago(60) as i64),
                detected_before: Some(days_ago(1) as i64),
                installed_after: Some(days_ago(30) as i64),
                installed_before: Some(days_ago(1) as i64),
            },
            patch_type: Some("OS".into()),
            statuses: Some(vec!["PENDING".into(), "FAILED".into()]),
            install_days: Some(30),
        }],
        auto_check_updates: true,
        actions: ActionSettings {
            enabled: true,
            os_patch_script_id: Some(42),
            software_patch_script_id: Some(43),
            ..ActionSettings::default()
        },
        tenant_changed: false,
    };

    let fixture = json!({
        "_comment": "Generated by fixtures::ipc_fixture_is_current (src-tauri/src/fixtures.rs). \
                     Do not edit by hand — see that test for why this exists.",
        "commands": {
            "auth_status": AuthStatus {
                authenticated: true,
                client_id: Some("fixture-client-id".into()),
                has_client_secret: true,
                instance_base_url: INSTANCE.into(),
                actions_enabled: true,
                write_enabled: true,
                scope_known: true,
            },
            "get_settings": settings,
            "list_orgs": orgs,
            "list_locations": locations,
            "list_roles": roles,
            "list_node_classes": list_node_classes(),
            "query_patches": QuerySummary::from_result(&result, 100),
            "get_patch_rows": page_rows(&result.rows, None, 0, 2),
            "get_patch_groups": slice_groups(&build_groups(&result.rows, GroupBy::Device), 0, 2),
            "get_device_rows": rows_by_device(&result.rows, &[1], 1),
            "device_detail": device_detail(&result, 1, 1),
            "plan_action": plan,
            "run_action": ActionBatch {
                batch_id: BATCH_ID,
                dispatched: 1,
                skipped: 1,
                jobs: jobs[..1].to_vec(),
            },
            "list_jobs": jobs,
            "list_scripts": [ScriptSummary {
                id: 42,
                name: "Install-SelectedPatches".into(),
                description: Some("Installs only the KBs passed in kbAllowList.".into()),
                language: Some("powershell".into()),
                operating_systems: vec!["WINDOWS".into()],
                accepts_kb_allow_list: true,
                accepts_dry_run: true,
            }],
            "list_run_as_options": RunAsOptions {
                roles: vec!["system".into(), "loggedonuser".into()],
            },
            "read_action_audit": [AuditRecord {
                timestamp: fmt_ts(FIXTURE_NOW - Duration::minutes(10)),
                kind: "OS_PATCH_REMEDIATE".into(),
                device_name: "web-01".into(),
                organization: "Contoso".into(),
                detail: "Apply selected OS patches (#42)".into(),
                outcome: "completed".into(),
                dry_run: false,
                window_override: true,
                batch_id: Some(BATCH_ID),
                exit_code: Some(0),
                legacy: false,
            }],
            "read_run_history": [RunRecord::from_result(&result, INSTANCE)],
            "check_for_update": Some(UpdateInfo {
                version: "1.4.0".into(),
                current_version: "1.3.2".into(),
                notes: Some("### Fixed\n- Example release note.".into()),
            }),
        },
        "events": {
            "query:progress": QueryProgressEvent {
                query_id: 3,
                stage: "osPatches",
                loaded: 500,
            },
            "action:progress": ActionProgressEvent {
                batch_id: BATCH_ID,
                stage: "polling",
                dispatched: 1,
                total: 2,
                jobs: jobs[1..2].to_vec(),
            },
        },
        "enums": {
            "actionKind": ActionKind::ALL,
            "jobState": JobState::ALL
                .iter()
                .map(|s| serde_json::to_value(s).expect("serialize a job state")["state"].clone())
                .collect::<Vec<_>>(),
        },
    });
    assert_every_field_is_exercised(&fixture);
    assert_fixture_current(IPC_FIXTURE, &fixture);
}
