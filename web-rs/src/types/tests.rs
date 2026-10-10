//! Holds every mirror in `types.rs` to the backend's real output.
//!
//! `web-rs/tests/backend-ipc.json` is written by the backend
//! (`fixtures::ipc_fixture_is_current` in `src-tauri/src/fixtures.rs`): one value of
//! every shape the frontend decodes, keyed by the command or event that carries it.
//! Each is decoded here through the type its `ipc!` wrapper or listener declares —
//! the check the backend's hand-typed key lists, which this replaced, never made.
//!
//! One gap is left by construction: these decode with `serde_json`, the app with
//! `serde_wasm_bindgen` from a JS value. The two read everything mirrored here the
//! same way except integers above 2^53, which a JS number cannot hold (no id or
//! count here comes near), and integral floats, which reach JS as plain numbers —
//! `serde_wasm_bindgen` would read `18.0` into an integer field where `serde_json`
//! refuses, so this side is the stricter one.

use std::collections::BTreeSet;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

use super::*;

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../tests/backend-ipc.json"))
        .expect("the committed backend fixture parses")
}

/// Decodes `sent` the way `api::invoke` does, then checks the mirror read back
/// exactly what was sent.
fn decode_and_compare<T: DeserializeOwned + Serialize>(what: &str, sent: &Value) {
    let decoded: T = serde_json::from_value(sent.clone()).unwrap_or_else(|e| {
        panic!("decode {what}: {e} — web-rs/src/types.rs no longer reads what the backend sends")
    });
    let read = serde_json::to_value(&decoded).expect("re-serialize the mirror");
    assert_mirrors(what, &read, sent);
}

/// Every key the mirror writes back must be one the backend sent, holding the value
/// the backend sent. Keys the mirror does not declare are ignored, as serde ignores
/// them in the app. A backend rename of a field the mirror marks `#[serde(default)]`
/// still decodes — to the default — so this is the check that catches it.
fn assert_mirrors(at: &str, read: &Value, sent: &Value) {
    match (read, sent) {
        (Value::Object(read), Value::Object(sent)) => {
            for (key, r) in read {
                let s = sent.get(key).unwrap_or_else(|| {
                    panic!(
                        "{at}.{key}: the mirror reads a key the backend does not send — \
                         renamed or dropped backend-side, and hidden by #[serde(default)]"
                    )
                });
                assert_mirrors(&format!("{at}.{key}"), r, s);
            }
        }
        (Value::Array(read), Value::Array(sent)) => {
            assert_eq!(read.len(), sent.len(), "{at}: element count");
            for (i, (r, s)) in read.iter().zip(sent).enumerate() {
                assert_mirrors(&format!("{at}[{i}]"), r, s);
            }
        }
        _ => assert_eq!(
            read, sent,
            "{at}: the mirror decoded a different value than the backend sent"
        ),
    }
}

type Check = fn(&str, &Value);

/// Asserts `entries` (an object of the fixture) and `checks` name the same keys,
/// then runs each check on its entry.
fn run_checks(section: &str, entries: &Value, checks: &[(&str, Check)]) {
    let entries = entries
        .as_object()
        .unwrap_or_else(|| panic!("the fixture's `{section}` is an object"));
    let sent: BTreeSet<&str> = entries.keys().map(String::as_str).collect();
    let checked: BTreeSet<&str> = checks.iter().map(|(name, _)| *name).collect();
    assert_eq!(
        checked, sent,
        "the backend fixture's `{section}` and this table must name the same entries — \
         each one the backend emits needs its mirror type here"
    );
    for (name, check) in checks {
        check(name, &entries[*name]);
    }
}

/// Every command result the frontend decodes, through the type its `ipc!` wrapper in
/// `api.rs` returns. A type change, an enum spelling or a rename hidden by
/// `#[serde(default)]` used to surface only as a "decode <cmd>" toast in a running
/// app; here it is a red test naming the command and the field. Commands that share
/// a return type with one listed (`get_patch_group_members`, `clear_jobs`,
/// `save_settings`, `save_preset`/`delete_preset` through `get_settings`'s presets)
/// are covered by it; the rest return `()`, a path or a bare `Option<String>`.
#[test]
fn every_command_result_decodes_through_its_mirror() {
    let checks: [(&str, Check); 19] = [
        ("auth_status", decode_and_compare::<AuthStatus>),
        ("get_settings", decode_and_compare::<SettingsView>),
        ("list_orgs", decode_and_compare::<Vec<Organization>>),
        ("list_locations", decode_and_compare::<Vec<Location>>),
        ("list_roles", decode_and_compare::<Vec<Role>>),
        ("list_node_classes", decode_and_compare::<Vec<NodeClass>>),
        ("query_patches", decode_and_compare::<QueryResult>),
        ("get_patch_rows", decode_and_compare::<Vec<PatchRow>>),
        ("get_patch_groups", decode_and_compare::<GroupPage>),
        ("get_device_rows", decode_and_compare::<Vec<DeviceRows>>),
        ("device_detail", decode_and_compare::<Option<DeviceDetail>>),
        ("plan_action", decode_and_compare::<ActionPlan>),
        ("run_action", decode_and_compare::<ActionBatch>),
        ("list_jobs", decode_and_compare::<Vec<JobReport>>),
        ("list_scripts", decode_and_compare::<Vec<ScriptSummary>>),
        ("list_run_as_options", decode_and_compare::<RunAsOptions>),
        ("read_action_audit", decode_and_compare::<Vec<AuditRecord>>),
        ("read_run_history", decode_and_compare::<Vec<RunRecord>>),
        ("check_for_update", decode_and_compare::<Option<UpdateInfo>>),
    ];
    run_checks("commands", &fixture()["commands"], &checks);
}

/// Both backend events, through the payload types `api::subscribe` decodes them
/// into. A listener drops a payload it cannot decode (it only logs a warning), so a
/// drifted event used to cost the progress bar or the live Jobs updates with no
/// visible error at all.
#[test]
fn both_progress_events_decode_through_their_mirrors() {
    let checks: [(&str, Check); 2] = [
        ("query:progress", decode_and_compare::<QueryProgressEvent>),
        ("action:progress", decode_and_compare::<ActionProgressEvent>),
    ];
    run_checks("events", &fixture()["events"], &checks);
}

/// The frontend's enum mirrors spell exactly the backend's variants — no more, no
/// fewer. Decoding the fixture's jobs already fails on a backend variant the mirror
/// lacks; this also fails on a mirror variant the backend no longer has, which would
/// otherwise sit unreachable while its label and styling rot. The backend's lists
/// come from its own `ALL` constants, each guarded by an exhaustive match.
#[test]
fn the_mirrored_enums_spell_exactly_the_backends_variants() {
    let fixture = fixture();
    let backend = |name: &str| -> BTreeSet<String> {
        fixture["enums"][name]
            .as_array()
            .unwrap_or_else(|| panic!("the fixture lists `{name}`"))
            .iter()
            .map(|v| v.as_str().expect("a variant spelling").to_owned())
            .collect()
    };
    let spelled = |v: Value| v.as_str().expect("a variant spelling").to_owned();

    let kinds: BTreeSet<String> = ActionKind::ALL
        .iter()
        .map(|k| spelled(serde_json::to_value(k).expect("serialize an action kind")))
        .collect();
    assert_eq!(kinds, backend("actionKind"), "ActionKind");

    let states: BTreeSet<String> = JobState::ALL
        .iter()
        .map(|s| spelled(serde_json::to_value(s).expect("serialize a job state")["state"].clone()))
        .collect();
    assert_eq!(states, backend("jobState"), "JobState");
}
