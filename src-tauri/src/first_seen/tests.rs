use super::*;
use crate::changes::patch_key;
use crate::filter::FilterParams;
use crate::model::{OsInfo, software_patch_json};
use crate::rows::{LookupMaps, PatchRecord, PatchSource, build_rows};

const TENANT: &str = "https://app.ninjarmm.com\nclient-a";
const DAY: i64 = 86_400;
const T0: i64 = 1_780_000_000;

fn os_patch(device_id: i64, kb: &str, status: &str) -> Patch {
    Patch {
        device_id: Some(device_id),
        kb_number: Some(kb.into()),
        name: Some(format!("Update {kb}")),
        version: None,
        product_vendor: None,
        severity: Some("CRITICAL".into()),
        status: Some(status.into()),
        patch_type: None,
        collected_timestamp: Some(T0 as f64),
        installed_timestamp: None,
        product_identifier: None,
    }
}

fn device(id: i64, offline: bool) -> Device {
    Device {
        id,
        system_name: Some(format!("srv{id}")),
        display_name: None,
        organization_id: Some(1),
        location_id: None,
        node_role_id: None,
        node_class: Some("WINDOWS_SERVER".into()),
        offline: Some(offline),
        os: Some(OsInfo {
            name: Some("Windows Server 2022".into()),
            needs_reboot: None,
        }),
        last_contact: None,
        pending_reboot_reason: None,
    }
}

fn first(l: &FamilyLedger, device_id: i64, p: &Patch) -> Option<i64> {
    let (mut key, mut name) = (String::new(), String::new());
    key_into(&mut key, &mut name, Family::Os, p);
    l.get(device_id, &key)
}

fn rebuilt(prev: Option<&FamilyLedger>, feed: &[Patch], at: i64, offline: &[i64]) -> Rebuilt {
    let offline: HashSet<i64> = offline.iter().copied().collect();
    rebuild(prev, feed, Family::Os, at, &offline).expect("a newer feed rebuilds")
}

/// A record keeps the fetch it first appeared in; a new one is stamped with this
/// fetch; a record no longer pending is dropped.
#[test]
fn records_keep_their_first_observation_and_resolved_ones_drop_out() {
    let a = os_patch(1, "KB1", "MANUAL");
    let b = os_patch(1, "KB2", "APPROVED");
    let day0 = rebuilt(None, std::slice::from_ref(&a), T0, &[]).section;
    assert_eq!(day0.since, T0);
    assert_eq!(first(&day0, 1, &a), Some(T0));

    let day5 = rebuilt(Some(&day0), &[a.clone(), b.clone()], T0 + 5 * DAY, &[]);
    assert!(day5.membership_changed);
    let day5 = day5.section;
    assert_eq!(first(&day5, 1, &a), Some(T0), "kept, not re-stamped");
    assert_eq!(first(&day5, 1, &b), Some(T0 + 5 * DAY));
    assert_eq!(day5.since, T0, "kept since the first merge");

    let installed = os_patch(1, "KB1", "INSTALLED");
    let day9 = rebuilt(Some(&day5), &[installed, b.clone()], T0 + 9 * DAY, &[]).section;
    assert_eq!(first(&day9, 1, &a), None, "installed: no longer pending");
    assert_eq!(first(&day9, 1, &b), Some(T0 + 5 * DAY));
}

/// The same feed again changes nothing worth writing.
#[test]
fn an_unchanged_feed_is_not_a_membership_change() {
    let a = os_patch(1, "KB1", "MANUAL");
    let day0 = rebuilt(None, std::slice::from_ref(&a), T0, &[]).section;
    let again = rebuilt(Some(&day0), &[a], T0 + DAY, &[]);
    assert!(!again.membership_changed);
    assert_eq!(again.section.merged_at, T0 + DAY);
}

/// A feed no newer than the last one merged is skipped, so an older run can never
/// roll the record back; and a feed with no pending record never wipes it.
#[test]
fn an_older_or_empty_feed_never_rewrites_the_record() {
    let a = os_patch(1, "KB1", "MANUAL");
    let day5 = rebuilt(None, std::slice::from_ref(&a), T0 + 5 * DAY, &[]).section;
    let offline = HashSet::new();
    assert!(rebuild(Some(&day5), &[], Family::Os, T0, &offline).is_none());
    assert!(
        rebuild(
            Some(&day5),
            std::slice::from_ref(&a),
            Family::Os,
            T0 + 5 * DAY,
            &offline
        )
        .is_none()
    );
    assert!(
        rebuild(Some(&day5), &[], Family::Os, T0 + 9 * DAY, &offline).is_none(),
        "a successful empty page must not prune everything"
    );
    let rejected = os_patch(1, "KB1", "REJECTED");
    assert!(rebuild(Some(&day5), &[rejected], Family::Os, T0 + 9 * DAY, &offline).is_none());
}

/// An offline device's absence from the feed proves nothing, so its records are
/// carried over; a device gone from the inventory loses them.
#[test]
fn an_offline_devices_records_survive_its_absence_from_the_feed() {
    let on_1 = os_patch(1, "KB1", "MANUAL");
    let on_2 = os_patch(2, "KB1", "MANUAL");
    let day0 = rebuilt(None, &[on_1.clone(), on_2.clone()], T0, &[]).section;
    let other = os_patch(3, "KB9", "MANUAL");
    let day5 = rebuilt(Some(&day0), &[other], T0 + 5 * DAY, &[1]).section;
    assert_eq!(first(&day5, 1, &on_1), Some(T0), "offline: carried over");
    assert_eq!(first(&day5, 2, &on_2), None, "online and absent: resolved");
}

/// Past the cap the newest observations go: the oldest carry the SLA signal.
#[test]
fn the_cap_keeps_the_oldest_observations() {
    let mut prev = FamilyLedger {
        since: T0,
        merged_at: T0,
        ..FamilyLedger::default()
    };
    let ix = prev.intern("OS|kbold");
    prev.entries.insert((1, ix), T0 - 100 * DAY);
    let feed: Vec<Patch> = (0..(MAX_ENTRIES_PER_FAMILY as i64 + 2))
        .map(|d| os_patch(d + 10, "KB2", "MANUAL"))
        .chain([Patch {
            kb_number: Some("KBOLD".into()),
            ..os_patch(1, "KBOLD", "MANUAL")
        }])
        .collect();
    let next = rebuilt(Some(&prev), &feed, T0 + DAY, &[]).section;
    assert_eq!(next.entries.len(), MAX_ENTRIES_PER_FAMILY);
    assert_eq!(next.get(1, "OS|kbold"), Some(T0 - 100 * DAY));
}

/// The ledger's identity is the changes diff's: the key built from a raw record
/// equals `changes::patch_key` of the row the join builds from it — OS by KB,
/// third-party by product.
#[test]
fn the_ledger_key_is_the_changes_diff_key() {
    let d = device(7, false);
    let by_id = HashMap::from([(7, &d)]);
    let maps = LookupMaps::build(&[], &[], &[]);
    let os = os_patch(7, "KB5040434", "MANUAL");
    let sw: Patch = serde_json::from_value(software_patch_json(
        7,
        "a1b2c3d4-0000-0000-0000-000000000001",
        "Google Chrome 141.0.7390.55",
        "CRITICAL",
        "MANUAL",
    ))
    .expect("a vendor-shaped third-party record");
    for (p, family, label) in [(&os, Family::Os, "OS"), (&sw, Family::Software, "SOFTWARE")] {
        let records = [PatchRecord::vendor(p)];
        let rows = build_rows(
            &by_id,
            &maps,
            &[PatchSource {
                records: &records,
                type_label: label,
                status_override: None,
                status_filter: None,
            }],
            &FilterParams::default().prepare(),
        );
        let (mut key, mut name) = (String::new(), String::new());
        key_into(&mut key, &mut name, family, p);
        assert_eq!(key, patch_key(&rows[0]), "{label}");
    }
}

/// A resolver finds what was recorded; a record it has never seen, a family with no
/// record, or a record with no device has no first observation.
#[test]
fn the_resolver_reads_what_was_recorded() {
    let a = os_patch(1, "KB1", "MANUAL");
    let ledger = TenantLedger {
        tenant: TENANT.into(),
        os: Some(Arc::new(
            rebuilt(None, std::slice::from_ref(&a), T0, &[]).section,
        )),
        sw: None,
    };
    let at = DateTime::from_timestamp(T0, 0);
    assert_eq!(ledger.resolver(Family::Os).first_observed(&a), at);
    assert_eq!(
        ledger
            .resolver(Family::Os)
            .first_observed(&os_patch(1, "KB2", "MANUAL")),
        None
    );
    assert_eq!(ledger.resolver(Family::Software).first_observed(&a), None);
    let orphan = Patch {
        device_id: None,
        ..a
    };
    assert_eq!(ledger.resolver(Family::Os).first_observed(&orphan), None);
}

/// "Kept since" is the later of the included families' starts, and this run's fetch
/// for a family with no record yet.
#[test]
fn tracking_since_is_the_newer_included_familys_start() {
    let section = |since| {
        Some(Arc::new(FamilyLedger {
            since,
            merged_at: since,
            ..FamilyLedger::default()
        }))
    };
    let ledger = TenantLedger {
        tenant: TENANT.into(),
        os: section(T0),
        sw: section(T0 + 3 * DAY),
    };
    let now = DateTime::from_timestamp(T0 + 10 * DAY, 0).unwrap();
    let at = |d| DateTime::from_timestamp(T0 + d * DAY, 0);
    assert_eq!(
        ledger.tracking_since([(Family::Os, true), (Family::Software, true)], now),
        at(3)
    );
    assert_eq!(
        ledger.tracking_since([(Family::Os, true), (Family::Software, false)], now),
        at(0)
    );
    assert_eq!(
        TenantLedger::empty(TENANT)
            .tracking_since([(Family::Os, true), (Family::Software, false)], now),
        Some(now)
    );
    assert_eq!(
        ledger.tracking_since([(Family::Os, false), (Family::Software, false)], now),
        None
    );
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("npt-first-seen-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// The file round-trips, is owner-only, drops keys nothing references, and is
/// only ever read back for its own tenant.
#[test]
fn the_ledger_round_trips_and_belongs_to_one_tenant() {
    let dir = temp_dir("roundtrip");
    let a = os_patch(1, "KB1", "MANUAL");
    let b = os_patch(2, "KB2", "MANUAL");
    let mut os = rebuilt(None, &[a.clone(), b.clone()], T0, &[]).section;
    os.intern("OS|kbunreferenced");
    let ledger = TenantLedger {
        tenant: TENANT.into(),
        os: Some(Arc::new(os)),
        sw: None,
    };
    assert!(save_to(&dir, &ledger));

    let loaded = load_from(&dir, TENANT).expect("its own tenant reads it");
    let section = loaded.os.as_deref().expect("os section");
    assert_eq!(first(section, 1, &a), Some(T0));
    assert_eq!(first(section, 2, &b), Some(T0));
    assert_eq!(section.keys.len(), 2, "unreferenced keys are not written");
    assert!(loaded.sw.is_none());
    assert_eq!(load_from(&dir, "https://other\nclient-b"), None);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join(file_name(TENANT));
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600,
            "it names every device's unpatched backlog"
        );
    }

    // A copied file never dates another tenant's patches.
    std::fs::copy(
        dir.join(file_name(TENANT)),
        dir.join(file_name("https://other\nclient-b")),
    )
    .unwrap();
    assert_eq!(load_from(&dir, "https://other\nclient-b"), None);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A damaged file reads as no record rather than misdating anything.
#[test]
fn a_damaged_ledger_reads_as_none() {
    let dir = temp_dir("damaged");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(file_name(TENANT));
    std::fs::write(&path, b"{not json").unwrap();
    assert_eq!(load_from(&dir, TENANT), None);
    let bad_index = serde_json::json!({
        "version": LEDGER_VERSION,
        "tenant": TENANT,
        "os": { "since": T0, "mergedAt": T0, "keys": ["OS|kb1"], "entries": [[1, 5, T0]] },
        "sw": null,
    });
    std::fs::write(&path, bad_index.to_string()).unwrap();
    assert_eq!(load_from(&dir, TENANT), None);
    let _ = std::fs::remove_dir_all(&dir);
}
